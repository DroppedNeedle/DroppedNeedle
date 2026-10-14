"""Admin defaults for playlist "Request all tracks" (#204) live in their own
config section, so saving them never touches the release-type policy."""

import json
from types import SimpleNamespace

from api.v1.schemas.settings import PlaylistTrackRequestSettings
from services.preferences_service import PreferencesService


def _prefs(tmp_path, config):
    path = tmp_path / "config.json"
    path.write_text(json.dumps(config))
    return PreferencesService(SimpleNamespace(config_file_path=path)), path


def test_defaults_when_unset(tmp_path):
    prefs, _path = _prefs(tmp_path, {})
    assert prefs.get_playlist_track_request_settings() == PlaylistTrackRequestSettings(
        mode="auto", compilation_policy="match_playlist"
    )


def test_round_trip_in_its_own_section(tmp_path):
    prefs, path = _prefs(tmp_path, {})
    _before, revision = prefs.get_preferences_with_revision()

    prefs.save_playlist_track_request_settings(
        PlaylistTrackRequestSettings(mode="review", compilation_policy="studio")
    )

    assert prefs.get_playlist_track_request_settings() == PlaylistTrackRequestSettings(
        mode="review", compilation_policy="studio"
    )
    stored = json.loads(path.read_text())
    assert stored["playlist_track_requests"] == {
        "mode": "review",
        "compilation_policy": "studio",
    }
    assert prefs.get_preferences_with_revision()[1] == revision


def test_invalid_stored_values_fall_back_to_defaults(tmp_path):
    prefs, _path = _prefs(
        tmp_path, {"playlist_track_requests": {"mode": "yolo", "compilation_policy": 3}}
    )
    assert prefs.get_playlist_track_request_settings() == PlaylistTrackRequestSettings()
