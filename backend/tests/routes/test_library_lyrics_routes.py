"""Route tests for GET /library/tracks/{file_id}/lyrics (local-library lyrics)."""

from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest
from fastapi import FastAPI
from mutagen.flac import FLAC
from mutagen.id3 import ID3, USLT

from api.v1.routes.library import router
from core.dependencies import get_local_lyrics_service
from core.exceptions import ResourceNotFoundError
from services.local_lyrics_service import LocalLyricsService
from tests.helpers import build_test_client, override_user_auth

FIXTURES = Path(__file__).resolve().parent.parent / "fixtures" / "library"


def _copy_fixture(tmp_path, name):
    dest = tmp_path / name
    dest.write_bytes((FIXTURES / name).read_bytes())
    return dest


@pytest.fixture
def mock_local_files():
    mock = MagicMock()
    mock.resolve_validated_path = AsyncMock()
    return mock


@pytest.fixture
def client(mock_local_files):
    app = FastAPI()
    app.include_router(router)
    # real lyrics service; only the file_id -> path resolution is mocked
    service = LocalLyricsService(mock_local_files)
    app.dependency_overrides[get_local_lyrics_service] = lambda: service
    override_user_auth(app)
    return build_test_client(app)


def test_lyrics_embedded_mp3_uslt_unsynced(client, mock_local_files, tmp_path):
    audio_path = _copy_fixture(tmp_path, "mp3_full_01.mp3")
    tags = ID3(audio_path)
    tags.add(USLT(encoding=3, lang="eng", desc="", text="Is it getting better\nOr do you feel the same"))
    tags.save(audio_path)
    mock_local_files.resolve_validated_path.return_value = audio_path

    resp = client.get("/library/tracks/f1/lyrics")

    assert resp.status_code == 200
    body = resp.json()
    assert set(body.keys()) == {"text", "is_synced", "lines"}
    assert body["is_synced"] is False
    assert body["lines"] == [
        {"text": "Is it getting better", "start_seconds": None},
        {"text": "Or do you feel the same", "start_seconds": None},
    ]
    assert body["text"] == "Is it getting better\nOr do you feel the same"
    mock_local_files.resolve_validated_path.assert_awaited_once_with("f1")


def test_lyrics_embedded_flac_lrc_synced(client, mock_local_files, tmp_path):
    audio_path = _copy_fixture(tmp_path, "flac_full_01.flac")
    audio = FLAC(audio_path)
    audio["LYRICS"] = "[ti:Airbag]\n[00:14.20]In the next world war\n[00:20.00]In a jackknifed juggernaut"
    audio.save()
    mock_local_files.resolve_validated_path.return_value = audio_path

    resp = client.get("/library/tracks/f2/lyrics")

    assert resp.status_code == 200
    body = resp.json()
    assert body["is_synced"] is True
    assert body["lines"][0] == {"text": "In the next world war", "start_seconds": 14.2}
    assert body["lines"][1]["start_seconds"] == 20.0


def test_lyrics_sidecar_lrc_wins_over_embedded(client, mock_local_files, tmp_path):
    audio_path = _copy_fixture(tmp_path, "flac_full_02.flac")
    audio = FLAC(audio_path)
    audio["LYRICS"] = "embedded fallback text"
    audio.save()
    (tmp_path / "flac_full_02.lrc").write_text(
        "[ar:Radiohead]\n[offset:+200]\n[00:10.00][01:00.00]Rain down\n[00:30.50]On me\n",
        encoding="utf-8",
    )
    mock_local_files.resolve_validated_path.return_value = audio_path

    resp = client.get("/library/tracks/f3/lyrics")

    assert resp.status_code == 200
    body = resp.json()
    assert body["is_synced"] is True
    # offset +200ms shifts earlier; multi-timestamp line appears twice, sorted
    assert body["lines"] == [
        {"text": "Rain down", "start_seconds": 9.8},
        {"text": "On me", "start_seconds": 30.3},
        {"text": "Rain down", "start_seconds": 59.8},
    ]


def test_lyrics_404_when_none_found(client, mock_local_files, tmp_path):
    audio_path = _copy_fixture(tmp_path, "flac_full_01.flac")  # no lyric tags, no sidecar
    mock_local_files.resolve_validated_path.return_value = audio_path

    resp = client.get("/library/tracks/f4/lyrics")

    assert resp.status_code == 404


def test_lyrics_404_when_file_id_unknown(client, mock_local_files):
    mock_local_files.resolve_validated_path.side_effect = ResourceNotFoundError("nope")

    resp = client.get("/library/tracks/missing/lyrics")

    assert resp.status_code == 404


def test_lyrics_403_when_path_outside_library(client, mock_local_files):
    mock_local_files.resolve_validated_path.side_effect = PermissionError("outside roots")

    resp = client.get("/library/tracks/f5/lyrics")

    assert resp.status_code == 403
