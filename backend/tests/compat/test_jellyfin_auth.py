"""T4.2 - Jellyfin auth + identity (AuthenticateByName, token dialects, gate)."""

import json

import pytest

from api.compat.jellyfin.models import SERVER_ID

pytestmark = pytest.mark.asyncio


def _jbody(resp):
    return json.loads(resp.content)


def _auth_header(token, client="pytest"):
    return {"Authorization": f'MediaBrowser Token="{token}", Client="{client}", DeviceId="d1"'}


async def test_system_info_public_has_version_and_stable_id(compat_env):
    r = compat_env.client.get("/jellyfin/System/Info/Public")
    assert r.status_code == 200
    body = _jbody(r)
    assert body["Version"] == "10.10.6"
    assert body["Id"] == SERVER_ID
    assert body["StartupWizardCompleted"] is True
    assert body["ProductName"] == "Jellyfin Server"


async def test_authenticate_by_name_issues_token(compat_env):
    r = compat_env.client.post(
        "/jellyfin/Users/AuthenticateByName",
        json={"Username": "alice", "Pw": compat_env.secret},
    )
    assert r.status_code == 200
    body = _jbody(r)
    assert body["AccessToken"] == compat_env.secret
    assert body["ServerId"] == SERVER_ID
    assert body["User"]["Name"] == "alice"
    assert body["User"]["ServerId"] == SERVER_ID


# Finamp hard-casts these; a missing/empty object crashes it with
# "type 'Null' is not a subtype of type 'bool'" (issue #144). Field lists
# verified against Finamp lib/models/jellyfin_models.dart.
_FINAMP_REQUIRED_CONFIGURATION = {
    "PlayDefaultAudioTrack": bool, "DisplayMissingEpisodes": bool,
    "SubtitleMode": str, "DisplayCollectionsView": bool,
    "EnableLocalPassword": bool, "HidePlayedInLatest": bool,
    "RememberAudioSelections": bool, "RememberSubtitleSelections": bool,
    "EnableNextEpisodeAutoPlay": bool,
}
_FINAMP_REQUIRED_SESSION = {
    "UserId": str, "LastActivityDate": str, "SupportsRemoteControl": bool,
    "IsActive": bool, "SupportsMediaControl": bool, "HasCustomDeviceName": bool,
}


async def test_authenticate_response_survives_finamp_parsing(compat_env):
    r = compat_env.client.post(
        "/jellyfin/Users/AuthenticateByName",
        json={"Username": "alice", "Pw": compat_env.secret},
        headers={"Authorization": 'MediaBrowser Client="Finamp", Device="iPhone", '
                                  'DeviceId="dev-1", Version="0.6.25"'},
    )
    assert r.status_code == 200
    body = _jbody(r)
    config = body["User"]["Configuration"]
    for field, typ in _FINAMP_REQUIRED_CONFIGURATION.items():
        assert isinstance(config.get(field), typ), field
    assert isinstance(body["User"]["HasConfiguredEasyPassword"], bool)
    session = body["SessionInfo"]
    for field, typ in _FINAMP_REQUIRED_SESSION.items():
        assert isinstance(session.get(field), typ), field
    assert session["UserId"] == body["User"]["Id"]
    assert session["Client"] == "Finamp"
    assert session["DeviceName"] == "iPhone"
    assert session["DeviceId"] == "dev-1"


async def test_users_me_includes_full_configuration(compat_env):
    r = compat_env.client.get(
        "/jellyfin/Users/Me", headers=_auth_header(compat_env.secret)
    )
    assert r.status_code == 200
    config = _jbody(r)["Configuration"]
    for field, typ in _FINAMP_REQUIRED_CONFIGURATION.items():
        assert isinstance(config.get(field), typ), field


async def test_lowercase_path_routes_case_insensitively(compat_env):
    # Feishin (and others) lowercase the whole URL path; real Jellyfin routes paths
    # case-insensitively, so /jellyfin/users/authenticatebyname must hit the handler.
    r = compat_env.client.post(
        "/jellyfin/users/authenticatebyname",
        json={"Username": "alice", "Pw": compat_env.secret},
    )
    assert r.status_code == 200
    assert _jbody(r)["AccessToken"] == compat_env.secret
    # a parameterised path keeps its id intact while the literal segments canonicalise
    pub = compat_env.client.get("/JELLYFIN/system/INFO/public")
    assert pub.status_code == 200
    assert _jbody(pub)["Id"] == SERVER_ID


async def test_authenticate_bad_pw_is_401(compat_env):
    r = compat_env.client.post(
        "/jellyfin/Users/AuthenticateByName",
        json={"Username": "alice", "Pw": "wrong"},
    )
    assert r.status_code == 401


async def test_lenient_auth_body_ignores_unknown_fields(compat_env):
    # Finamp-style extra field must not 400
    r = compat_env.client.post(
        "/jellyfin/Users/AuthenticateByName",
        json={"Username": "alice", "Pw": compat_env.secret, "UserId": "x", "App": "Finamp"},
    )
    assert r.status_code == 200


async def test_token_round_trip_via_header(compat_env):
    token = _jbody(compat_env.client.post(
        "/jellyfin/Users/AuthenticateByName",
        json={"Username": "alice", "Pw": compat_env.secret}))["AccessToken"]
    r = compat_env.client.get("/jellyfin/Users/Me", headers=_auth_header(token))
    assert r.status_code == 200
    assert _jbody(r)["Name"] == "alice"


async def test_user_policy_grants_library_access(compat_env):
    # Manet reads Policy.EnableAllFolders/EnabledFolders off the user object to decide
    # whether any libraries are accessible; without them it never calls /UserViews and
    # reports "No music libraries found".
    me = _jbody(compat_env.client.get("/jellyfin/Users/Me", headers=_auth_header(compat_env.secret)))
    assert me["Policy"]["EnableAllFolders"] is True
    assert me["Policy"]["EnabledFolders"] == []
    assert me["Policy"]["EnableMediaPlayback"] is True


async def test_token_via_x_emby_and_apikey_query(compat_env):
    token = compat_env.secret
    r1 = compat_env.client.get("/jellyfin/Users/Me", headers={"X-Emby-Token": token})
    assert r1.status_code == 200
    r2 = compat_env.client.get("/jellyfin/Users/Me", params={"ApiKey": token})
    assert r2.status_code == 200


async def test_lenient_authorization_header_with_extra_fields(compat_env):
    # missing optional fields + Finamp's non-standard UserId still authenticates
    h = {"Authorization": f'MediaBrowser Token="{compat_env.secret}", UserId="abc"'}
    r = compat_env.client.get("/jellyfin/Users/Me", headers=h)
    assert r.status_code == 200


async def test_missing_token_is_401(compat_env):
    r = compat_env.client.get("/jellyfin/Users/Me")
    assert r.status_code == 401


async def test_quick_connect_disabled(compat_env):
    r = compat_env.client.get("/jellyfin/QuickConnect/Enabled")
    assert r.status_code == 200
    assert r.content == b"false"


async def test_logout_no_op_204(compat_env):
    r = compat_env.client.post("/jellyfin/Sessions/Logout")
    assert r.status_code == 204


async def test_system_info_authenticated(compat_env):
    r = compat_env.client.get("/jellyfin/System/Info", headers=_auth_header(compat_env.secret))
    assert r.status_code == 200
    assert _jbody(r)["SupportsLibraryMonitor"] is True


async def test_disabled_protocol_404_on_public_info(compat_env):
    from api.v1.schemas.settings import ConnectAppsSettings

    compat_env.preferences.save_connect_apps_settings(
        ConnectAppsSettings(jellyfin_enabled=False)
    )
    r = compat_env.client.get("/jellyfin/System/Info/Public")
    assert r.status_code == 404  # clients treat the server as absent
