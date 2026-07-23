import inspect
from pathlib import Path
import shutil

import mutagen
import pytest

from api.v1.schemas.library_management import AUDIO_MANAGED_FIELD_NAMES
from core.exceptions import AudioFormatMismatchError, UnsupportedAudioFormatError
from infrastructure.audio.metadata_engine import (
    AUDIO_EXTENSION_FORMATS,
    FORMAT_CAPABILITIES,
    AudioMetadataEngine,
    _ADAPTERS,
    _MappedAdapter,
    legacy_audio_projection,
)
from infrastructure.audio.protocols import AudioReadAdapterProtocol
from infrastructure.audio.tagger import AudioTagger
from services.local_files_service import AUDIO_EXTENSIONS

FIXTURES = Path(__file__).parents[1] / "fixtures" / "library"
FORMATS = ("flac", "mp3", "ogg", "opus", "m4a", "aac", "wav", "wma")


@pytest.mark.parametrize("audio_format", FORMATS)
def test_explicit_adapters_read_rich_independent_fixtures(audio_format: str) -> None:
    path = FIXTURES / f"management_full.{audio_format}"
    document = AudioMetadataEngine().read(path)

    assert document.probe.detected_format == audio_format
    assert document.probe.detected_class
    assert document.probe.extension_matches is True
    assert document.metadata.value_for("title") == "Management Track"
    assert document.metadata.value_for("title_sort") == "Management Track, The"
    assert document.metadata.value_for("album_sort") == "Management Album, The"
    assert document.metadata.strings_for("artist") == ("Alpha", "Beta")
    assert document.metadata.strings_for("genre") == ("Electronic", "Ambient")
    assert document.metadata.value_for("musicbrainz_recording_id") == (
        "10000000-0000-4000-8000-000000000003"
    )
    assert document.metadata.value_for("musicbrainz_release_track_id") == (
        "10000000-0000-4000-8000-000000000004"
    )
    assert document.metadata.value_for("musicbrainz_recording_id") != (
        document.metadata.value_for("musicbrainz_release_track_id")
    )
    legacy_tag, _ = legacy_audio_projection(document)
    assert legacy_tag.musicbrainz_release_track_id == (
        "10000000-0000-4000-8000-000000000004"
    )
    assert any("custom_keep" in value.key.casefold() for value in document.raw_tags)
    assert len(document.artwork) == 1
    assert document.artwork[0].image_type == "front"
    assert document.artwork[0].mime_type == "image/png"
    assert (document.artwork[0].width, document.artwork[0].height) == (4, 3)
    assert document.artwork[0].format_supported is True
    assert document.technical.duration_seconds > 0
    assert document.technical.sample_rate_hz == (
        48_000 if audio_format == "opus" else 44_100
    )
    assert document.technical.channels == 2
    assert document.technical.file_size_bytes == path.stat().st_size


def test_capability_registry_exactly_covers_admitted_extensions_and_fields() -> None:
    assert set(AUDIO_EXTENSION_FORMATS) == AUDIO_EXTENSIONS
    assert set(AUDIO_EXTENSION_FORMATS.values()) == set(FORMAT_CAPABILITIES)
    assert set(_ADAPTERS) == set(FORMAT_CAPABILITIES)
    assert len({id(adapter) for adapter in _ADAPTERS.values()}) == len(_ADAPTERS)

    for audio_format, capabilities in FORMAT_CAPABILITIES.items():
        assert capabilities.audio_format == audio_format
        assert capabilities.readable is True
        assert capabilities.writable is True
        assert set(capabilities.supported_fields).isdisjoint(
            capabilities.unsupported_fields
        )
        assert set(capabilities.supported_fields) | set(
            capabilities.unsupported_fields
        ) == set(AUDIO_MANAGED_FIELD_NAMES)
        assert capabilities.extensions == tuple(
            extension
            for extension, mapped_format in AUDIO_EXTENSION_FORMATS.items()
            if mapped_format == audio_format
        )


def test_adapter_protocol_uses_runtime_comparable_annotations() -> None:
    assert inspect.signature(_MappedAdapter.read) == inspect.signature(
        AudioReadAdapterProtocol.read
    )


def test_read_never_calls_generic_mutagen_dispatch(monkeypatch) -> None:
    def fail_generic_dispatch(_path):
        raise AssertionError("generic mutagen.File dispatch was used")

    monkeypatch.setattr(mutagen, "File", fail_generic_dispatch)

    document = AudioMetadataEngine().read(FIXTURES / "management_full.aac")

    assert document.probe.detected_format == "aac"
    assert document.metadata.value_for("title") == "Management Track"


def test_suffix_container_mismatch_is_reported_and_blocked(tmp_path: Path) -> None:
    mismatched = tmp_path / "wrong.mp3"
    shutil.copyfile(FIXTURES / "management_full.flac", mismatched)
    engine = AudioMetadataEngine()

    report = engine.capabilities(mismatched)

    assert report.probe.detected_format == "flac"
    assert report.probe.extension_matches is False
    assert report.blockers == ("file extension does not match the detected container",)
    with pytest.raises(AudioFormatMismatchError):
        engine.read(mismatched)


def test_corrupt_admitted_file_is_blocked_before_adapter_open(tmp_path: Path) -> None:
    corrupt = tmp_path / "corrupt.flac"
    corrupt.write_bytes(b"not an audio container")
    engine = AudioMetadataEngine()

    report = engine.capabilities(corrupt)

    assert report.probe.admitted is True
    assert report.probe.detected_format is None
    assert report.blockers == ("audio container could not be detected",)
    with pytest.raises(UnsupportedAudioFormatError):
        engine.read(corrupt)


@pytest.mark.parametrize("audio_format", FORMATS)
def test_legacy_tagger_projection_keeps_scan_behavior(audio_format: str) -> None:
    tag, info = AudioTagger().read_tags(FIXTURES / f"management_full.{audio_format}")

    assert tag.title == "Management Track"
    assert tag.artist == "Alpha feat. Beta"
    assert tag.artists[0].name == "Alpha"
    assert tag.genres == ["Electronic", "Ambient"]
    assert tag.title_sort == "Management Track, The"
    assert tag.album_sort == "Management Album, The"
    assert info.file_format == audio_format
    assert info.duration_seconds > 0
