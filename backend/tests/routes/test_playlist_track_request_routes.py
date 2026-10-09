"""Routes for "Request all tracks" (#204), end to end over a real playlist DB:
ownership, background jobs + SSE outcome, review vs auto, choose-home, and
the defaults endpoint."""

import asyncio
import threading
from unittest.mock import AsyncMock

import pytest
from fastapi import FastAPI
from fastapi.testclient import TestClient

import api.v1.routes.playlists as playlist_routes
from api.v1.schemas.request import BatchRequestResponse
from api.v1.schemas.settings import PlaylistTrackRequestSettings
from core.dependencies import get_playlist_service
from core.exception_handlers import (
    general_exception_handler,
    resource_not_found_handler,
    validation_error_handler,
)
from core.exceptions import ResourceNotFoundError, ValidationError
from core.task_registry import TaskRegistry
from models.release_home import RecordingHome
from repositories.playlist_repository import PlaylistRepository, TrackResolution
from services.playlist_service import PlaylistService
from services.playlist_track_request_service import PlaylistTrackRequestService
from services.playlist_track_resolver import PlaylistTrackResolver
from tests.helpers import override_user_auth

OWNER = "test-user-id"


def _home(rg, title, secondary=()):
    return RecordingHome(
        release_group_mbid=rg,
        title=title,
        primary_type="Album",
        secondary_types=secondary,
        official=True,
        is_various=False,
        artist_credit="Creedence Clearwater Revival",
        first_release_date="1969-01-01",
        artist_mbids=("ccr",),
    )


class _Mb:
    async def get_recording_homes(self, recording_mbid, *, priority=None):
        return {
            "rec-fs": [
                _home("rg-willy", "Willy"),
                _home("rg-chr", "Chronicle", ("Compilation",)),
            ],
            "rec-pm": [_home("rg-bayou", "Bayou Country")],
        }.get(recording_mbid, [])

    async def get_artist_release_groups(self, *args, **kwargs):
        return [], 0


class _Sse:
    def __init__(self):
        self.events: list[tuple[str, str, dict]] = []
        self.done = threading.Event()

    async def publish(self, channel, event, data):
        self.events.append((channel, event, data))
        self.done.set()


class _Prefs:
    def __init__(self):
        self.settings = PlaylistTrackRequestSettings()

    def get_playlist_track_request_settings(self):
        return self.settings


@pytest.fixture
def env(tmp_path, monkeypatch):
    TaskRegistry.get_instance().reset()
    repo = PlaylistRepository(
        db_path=tmp_path / "library.db", write_lock=threading.Lock()
    )
    playlists = PlaylistService(repo=repo, cache_dir=tmp_path)
    requests = AsyncMock()
    requests.request_track_batch.return_value = BatchRequestResponse(
        success=True, message="Track requests accepted: 2 started", requested=2
    )
    prefs = _Prefs()
    sse = _Sse()
    monkeypatch.setattr(playlist_routes, "get_sse_publisher", lambda: sse)

    def service():
        return PlaylistTrackRequestService(
            playlist_repo=playlists.repository,
            resolver_factory=lambda: PlaylistTrackResolver(
                playlist_repo=playlists.repository,
                mb_repo=_Mb(),
                album_service=AsyncMock(),
            ),
            request_service=requests,
            preferences_service=prefs,
        )

    app = FastAPI()
    app.include_router(playlist_routes.router)
    app.dependency_overrides[get_playlist_service] = lambda: playlists
    app.dependency_overrides[playlist_routes.get_playlist_track_request_service] = (
        service
    )
    override_user_auth(app, user_id=OWNER)
    app.add_exception_handler(ResourceNotFoundError, resource_not_found_handler)
    app.add_exception_handler(ValidationError, validation_error_handler)
    app.add_exception_handler(Exception, general_exception_handler)
    with TestClient(app) as client:
        yield client, app, playlists, requests, prefs, sse
    TaskRegistry.get_instance().reset()


async def _seed(playlists, owner=OWNER):
    record = await playlists.repository.create_playlist("CCR", user_id=owner)
    await playlists.repository.add_tracks(
        record.id,
        [
            {
                "track_name": "Fortunate Son",
                "artist_name": "Creedence Clearwater Revival",
                "album_name": "Chronicle",
                "source_type": "",
                "recording_mbid": "rec-fs",
                "duration": 140,
            },
            {
                "track_name": "Proud Mary",
                "artist_name": "Creedence Clearwater Revival",
                "album_name": "Chronicle",
                "source_type": "",
                "recording_mbid": "rec-pm",
            },
            {
                "track_name": "Owned",
                "artist_name": "Creedence Clearwater Revival",
                "album_name": "Chronicle",
                "source_type": "",
                "recording_mbid": "rec-fs",
                "available_sources": ["navidrome"],
            },
        ],
    )
    return record.id


def _seed_sync(client, playlists, owner=OWNER):
    return client.portal.call(_seed, playlists, owner)


def _wait(sse):
    assert sse.done.wait(5), "background job never signalled completion"
    return sse.events[-1]


def test_auto_mode_resolves_then_requests_each_missing_track(env):
    client, _app, playlists, requests, _prefs, sse = env
    pid = _seed_sync(client, playlists)

    resp = client.post(f"/playlists/{pid}/request-tracks", json={"mode": "auto"})

    assert resp.status_code == 202
    assert resp.json() == {
        "playlist_id": pid,
        "status": "started",
        "job": "resolve_and_request",
    }
    channel, event, data = _wait(sse)
    assert (channel, event) == (f"user:{OWNER}", "playlist_tracks_requested")
    assert data["summary"]["albums_by_tier"] == {"album": 2}
    assert data["summary"]["owned"] == 1
    assert data["result"]["requested"] == 2
    items = requests.request_track_batch.await_args.args[0]
    assert sorted((i["recording_mbid"], i["release_group_mbid"]) for i in items) == [
        ("rec-fs", "rg-willy"),
        ("rec-pm", "rg-bayou"),
    ]
    kwargs = requests.request_track_batch.await_args.kwargs
    assert (kwargs["user_id"], kwargs["user_role"]) == (OWNER, "user")


def test_review_flow_resolves_then_requests_only_on_confirm(env):
    client, _app, playlists, requests, _prefs, sse = env
    pid = _seed_sync(client, playlists)

    resp = client.post(f"/playlists/{pid}/resolve-tracks", json={})
    assert resp.json()["job"] == "resolve"
    _channel, event, data = _wait(sse)
    assert event == "playlist_tracks_resolved"
    assert data["summary"]["resolved"] == 2
    assert "result" not in data
    requests.request_track_batch.assert_not_awaited()

    tracks = client.get(f"/playlists/{pid}").json()["tracks"]
    assert [t["target_release_group_mbid"] for t in tracks] == [
        "rg-willy",
        "rg-bayou",
        None,
    ]
    assert tracks[0]["home_tier"] == "album"

    sse.done.clear()
    resp = client.post(f"/playlists/{pid}/request-tracks", json={"mode": "review"})
    assert resp.json()["job"] == "request"
    _channel, event, data = _wait(sse)
    assert event == "playlist_tracks_requested"
    assert "summary" not in data
    requests.request_track_batch.assert_awaited_once()


def test_defaults_drive_omitted_fields(env):
    client, _app, playlists, _requests, prefs, sse = env
    prefs.settings = PlaylistTrackRequestSettings(
        mode="review", compilation_policy="flag"
    )
    pid = _seed_sync(client, playlists)

    assert client.get("/playlists/track-request-settings").json() == {
        "mode": "review",
        "compilation_policy": "flag",
    }
    assert (
        client.post(f"/playlists/{pid}/request-tracks", json={}).json()["job"]
        == "request"
    )
    _channel, _event, data = _wait(sse)
    # Nothing was resolved yet, so a review-mode request has nothing to send.
    assert data["result"]["status"] == "nothing_to_request"


def test_second_click_while_running_is_a_no_op(env):
    client, _app, playlists, requests, _prefs, sse = env
    pid = _seed_sync(client, playlists)
    gate = threading.Event()

    async def slow(*args, **kwargs):
        while not gate.is_set():
            await asyncio.sleep(0.01)
        return BatchRequestResponse(success=True, message="ok")

    requests.request_track_batch.side_effect = slow
    first = client.post(f"/playlists/{pid}/request-tracks", json={"mode": "auto"})
    second = client.post(f"/playlists/{pid}/request-tracks", json={"mode": "auto"})
    gate.set()
    _wait(sse)

    assert first.json()["status"] == "started"
    assert second.json()["status"] == "already_running"


def test_other_users_playlist_is_refused(env):
    client, _app, playlists, requests, _prefs, sse = env
    private = _seed_sync(client, playlists, owner="someone-else")

    for path, body in (
        (f"/playlists/{private}/request-tracks", {"mode": "auto"}),
        (f"/playlists/{private}/resolve-tracks", {}),
        (
            f"/playlists/{private}/tracks/t-1/choose-home",
            {"recording_mbid": "r", "release_group_mbid": "rg"},
        ),
    ):
        assert client.post(path, json=body).status_code == 404  # private: not leaked

    async def make_public():
        await playlists.repository.set_public(private, True)

    client.portal.call(make_public)
    assert (
        client.post(
            f"/playlists/{private}/request-tracks", json={"mode": "auto"}
        ).status_code
        == 403
    )
    assert not sse.events
    requests.request_track_batch.assert_not_awaited()


def test_choose_home_accepts_only_offered_options(env):
    client, _app, playlists, _requests, _prefs, _sse = env
    pid = _seed_sync(client, playlists)
    options = [
        {
            "kind": "studio",
            "recording_mbid": "rec-st",
            "release_group_mbid": "rg-willy",
            "title": "Willy",
            "tier": "album",
        },
        {
            "kind": "compilation",
            "recording_mbid": "rec-fs",
            "release_group_mbid": "rg-chr",
            "title": "Chronicle",
            "tier": "artist_compilation",
        },
    ]

    async def flag():
        tracks = await playlists.repository.get_tracks(pid)
        await playlists.repository.batch_update_resolutions(
            pid,
            {
                tracks[0].id: TrackResolution(
                    None, None, "flagged", flag_options=options
                )
            },
        )
        return tracks[0].id

    track_id = client.portal.call(flag)

    bad = client.post(
        f"/playlists/{pid}/tracks/{track_id}/choose-home",
        json={"recording_mbid": "rec-st", "release_group_mbid": "rg-chr"},
    )
    assert bad.status_code == 400

    ok = client.post(
        f"/playlists/{pid}/tracks/{track_id}/choose-home",
        json={"recording_mbid": "REC-ST", "release_group_mbid": "rg-willy"},
    )
    assert ok.status_code == 200
    body = ok.json()
    assert (
        body["resolution_status"],
        body["recording_mbid"],
        body["target_release_group_mbid"],
        body["home_tier"],
    ) == (
        "chosen",
        "rec-st",
        "rg-willy",
        "album",
    )
    assert body["flag_options"] == options


def test_job_failure_is_reported_not_raised(env):
    client, _app, playlists, requests, _prefs, sse = env
    pid = _seed_sync(client, playlists)
    requests.request_track_batch.side_effect = RuntimeError("boom")

    client.post(f"/playlists/{pid}/request-tracks", json={"mode": "auto"})

    _channel, event, data = _wait(sse)
    assert event == "playlist_tracks_failed"
    assert "boom" not in data["message"]
