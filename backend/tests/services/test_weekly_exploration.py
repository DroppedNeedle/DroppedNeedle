"""Tests for ListenBrainz weekly exploration (recommendation playlists)."""
import asyncio

import pytest
from unittest.mock import AsyncMock, MagicMock

import httpx

from repositories.listenbrainz_repository import ListenBrainzRepository
from repositories.listenbrainz_models import (
    parse_recommendation_track,
    ListenBrainzRecommendationTrack,
)


def _make_repo(username: str = "testuser", user_token: str = "tok-abc"):
    http_client = AsyncMock(spec=httpx.AsyncClient)
    cache = MagicMock()
    cache.get = AsyncMock(return_value=None)
    cache.set = AsyncMock()
    return ListenBrainzRepository(http_client, cache, username, user_token), http_client


def _ok_response(json_data=None):
    resp = MagicMock()
    resp.status_code = 200
    resp.content = b""
    resp.json.return_value = json_data or {}
    resp.text = ""
    import msgspec
    if json_data is not None:
        resp.content = msgspec.json.encode(json_data)
    return resp


SAMPLE_PLAYLISTS_RESPONSE = {
    "count": 0,
    "offset": 0,
    "playlist_count": 2,
    "playlists": [
        {
            "playlist": {
                "identifier": "https://listenbrainz.org/playlist/abc-123",
                "title": "Weekly Exploration for testuser, week of 2026-03-23 Mon",
                "date": "2026-03-23T00:25:21.141222+00:00",
                "creator": "listenbrainz",
                "track": [],
                "extension": {
                    "https://musicbrainz.org/doc/jspf#playlist": {
                        "additional_metadata": {
                            "algorithm_metadata": {
                                "source_patch": "weekly-exploration"
                            }
                        },
                        "created_for": "testuser",
                        "creator": "listenbrainz",
                    }
                },
            }
        },
        {
            "playlist": {
                "identifier": "https://listenbrainz.org/playlist/def-456",
                "title": "Weekly Exploration for testuser, week of 2026-03-16 Mon",
                "date": "2026-03-16T00:24:27.677965+00:00",
                "creator": "listenbrainz",
                "track": [],
                "extension": {
                    "https://musicbrainz.org/doc/jspf#playlist": {
                        "additional_metadata": {
                            "algorithm_metadata": {
                                "source_patch": "weekly-exploration"
                            }
                        },
                    }
                },
            }
        },
    ],
}


SAMPLE_PLAYLIST_DETAIL = {
    "playlist": {
        "identifier": "https://listenbrainz.org/playlist/abc-123",
        "title": "Weekly Exploration for testuser, week of 2026-03-23 Mon",
        "date": "2026-03-23T00:25:21.141222+00:00",
        "creator": "listenbrainz",
        "extension": {
            "https://musicbrainz.org/doc/jspf#playlist": {
                "additional_metadata": {
                    "algorithm_metadata": {"source_patch": "weekly-exploration"}
                },
            }
        },
        "track": [
            {
                "title": "Eye in the Sky",
                "creator": "The Alan Parsons Project",
                "album": "Eye in the Sky",
                "duration": 276173,
                "identifier": [
                    "https://musicbrainz.org/recording/e9209cce-7b98-4d7e-ba62-779374272de6"
                ],
                "extension": {
                    "https://musicbrainz.org/doc/jspf#track": {
                        "additional_metadata": {
                            "artists": [
                                {
                                    "artist_credit_name": "The Alan Parsons Project",
                                    "artist_mbid": "f98711e5-06f7-43ed-8239-da0f61a9c460",
                                    "join_phrase": "",
                                }
                            ],
                            "caa_id": 14860133678,
                            "caa_release_mbid": "b43d2acd-4490-4517-b049-56e780cd0e69",
                        },
                    }
                },
            },
            {
                "title": "Wet",
                "creator": "Dazey and the Scouts",
                "album": "Maggot",
                "duration": 170000,
                "identifier": [
                    "https://musicbrainz.org/recording/a30c907d-0c8b-4841-902e-012ca67d08c2"
                ],
                "extension": {
                    "https://musicbrainz.org/doc/jspf#track": {
                        "additional_metadata": {
                            "artists": [
                                {
                                    "artist_credit_name": "Dazey and the Scouts",
                                    "artist_mbid": "a7fd3cbf-15cc-4a8a-b59f-587b5784feb2",
                                    "join_phrase": "",
                                }
                            ],
                            "caa_id": 19535225900,
                            "caa_release_mbid": "aaf9fb14-d3af-43d7-b6f3-882c520aa6f6",
                        },
                    }
                },
            },
        ],
    }
}


class TestParseRecommendationTrack:
    def test_parses_valid_track(self):
        raw = SAMPLE_PLAYLIST_DETAIL["playlist"]["track"][0]
        track = parse_recommendation_track(raw)
        assert track is not None
        assert track.title == "Eye in the Sky"
        assert track.creator == "The Alan Parsons Project"
        assert track.album == "Eye in the Sky"
        assert track.recording_mbid == "e9209cce-7b98-4d7e-ba62-779374272de6"
        assert track.artist_mbids == ["f98711e5-06f7-43ed-8239-da0f61a9c460"]
        assert track.duration_ms == 276173
        assert track.caa_id == 14860133678
        assert track.caa_release_mbid == "b43d2acd-4490-4517-b049-56e780cd0e69"

    def test_returns_none_for_missing_title(self):
        track = parse_recommendation_track({"creator": "Artist"})
        assert track is None

    def test_returns_none_for_missing_creator(self):
        track = parse_recommendation_track({"title": "Song"})
        assert track is None

    def test_handles_missing_extension(self):
        track = parse_recommendation_track({
            "title": "Song",
            "creator": "Artist",
            "album": "Album",
        })
        assert track is not None
        assert track.artist_mbids is None
        assert track.caa_id is None
        assert track.duration_ms is None

    def test_handles_non_numeric_duration(self):
        track = parse_recommendation_track({
            "title": "Song",
            "creator": "Artist",
            "album": "Album",
            "duration": "invalid",
        })
        assert track is not None
        assert track.duration_ms is None


class TestGetRecommendationPlaylists:
    @pytest.mark.asyncio
    async def test_returns_playlists(self):
        repo, http = _make_repo()
        http.request = AsyncMock(return_value=_ok_response(SAMPLE_PLAYLISTS_RESPONSE))
        result = await repo.get_recommendation_playlists()
        assert len(result) == 2
        assert result[0]["playlist_id"] == "abc-123"
        assert result[0]["source_patch"] == "weekly-exploration"
        assert result[1]["playlist_id"] == "def-456"

    @pytest.mark.asyncio
    async def test_returns_empty_for_no_username(self):
        repo, _ = _make_repo(username="")
        result = await repo.get_recommendation_playlists()
        assert result == []

    @pytest.mark.asyncio
    async def test_returns_empty_for_404(self):
        repo, http = _make_repo()
        resp = MagicMock()
        resp.status_code = 404
        resp.text = "Not Found"
        http.request = AsyncMock(return_value=resp)
        result = await repo.get_recommendation_playlists()
        assert result == []

    @pytest.mark.asyncio
    async def test_caches_result(self):
        repo, http = _make_repo()
        http.request = AsyncMock(return_value=_ok_response(SAMPLE_PLAYLISTS_RESPONSE))
        await repo.get_recommendation_playlists()
        repo._cache.set.assert_called_once()
        call_args = repo._cache.set.call_args
        assert call_args[0][0] == "lb_rec_playlists:testuser"
        assert call_args[1]["ttl_seconds"] == 21600


class TestGetPlaylistTracks:
    @pytest.mark.asyncio
    async def test_returns_playlist_with_tracks(self):
        repo, http = _make_repo()
        http.request = AsyncMock(return_value=_ok_response(SAMPLE_PLAYLIST_DETAIL))
        result = await repo.get_playlist_tracks("abc-123")
        assert result is not None
        assert len(result.tracks) == 2
        assert result.tracks[0].title == "Eye in the Sky"
        assert result.tracks[1].title == "Wet"
        assert result.source_patch == "weekly-exploration"

    @pytest.mark.asyncio
    async def test_returns_none_for_empty_id(self):
        repo, _ = _make_repo()
        result = await repo.get_playlist_tracks("")
        assert result is None

    @pytest.mark.asyncio
    async def test_returns_none_for_404(self):
        repo, http = _make_repo()
        resp = MagicMock()
        resp.status_code = 404
        resp.text = "Not Found"
        http.request = AsyncMock(return_value=resp)
        result = await repo.get_playlist_tracks("nonexistent")
        assert result is None

    @pytest.mark.asyncio
    async def test_caches_playlist(self):
        repo, http = _make_repo()
        http.request = AsyncMock(return_value=_ok_response(SAMPLE_PLAYLIST_DETAIL))
        await repo.get_playlist_tracks("abc-123")
        repo._cache.set.assert_called_once()
        call_args = repo._cache.set.call_args
        assert call_args[0][0] == "lb_rec_playlist:abc-123"
        assert call_args[1]["ttl_seconds"] == 21600


class TestRecordingMetadataBatch:
    @pytest.mark.asyncio
    async def test_resolves_release_groups_in_one_listenbrainz_request(self):
        repo, http = _make_repo()
        http.request = AsyncMock(
            return_value=_ok_response(
                {
                    "rec-1": {
                        "release": {"release_group_mbid": "rg-1"},
                    },
                    "rec-2": {
                        "release": {"release_group_mbid": "rg-2"},
                    },
                }
            )
        )

        result = await repo.get_recording_release_groups_batch(
            ["rec-1", "rec-2", "rec-1"]
        )

        assert result == {"rec-1": "rg-1", "rec-2": "rg-2"}
        http.request.assert_awaited_once()
        request = http.request.await_args
        assert request.args[:2] == ("POST", "https://api.listenbrainz.org/1/metadata/recording/")
        assert request.kwargs["json"] == {
            "recording_mbids": ["rec-1", "rec-2"],
            "inc": "release",
        }

    @pytest.mark.asyncio
    async def test_negative_metadata_result_is_cached(self):
        repo, http = _make_repo()
        http.request = AsyncMock(return_value=_ok_response({"rec-1": {}}))

        assert await repo.get_recording_release_groups_batch(["rec-1"]) == {}

        repo._cache.set.assert_awaited_once_with(
            "lb_recording_release_group:rec-1",
            "",
            ttl_seconds=86400,
        )

    @pytest.mark.asyncio
    async def test_unavailable_metadata_response_is_not_negative_cached(self):
        repo, _ = _make_repo()
        repo._post = AsyncMock(return_value=None)

        assert await repo.get_recording_release_groups_batch(["rec-1"]) == {}

        repo._cache.set.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_concurrent_identical_batches_share_one_request(self):
        repo, _ = _make_repo()
        request_started = asyncio.Event()
        allow_response = asyncio.Event()

        async def fetch_metadata(*args, **kwargs):
            request_started.set()
            await allow_response.wait()
            return {"rec-1": {"release": {"release_group_mbid": "rg-1"}}}

        repo._post = AsyncMock(side_effect=fetch_metadata)
        first = asyncio.create_task(
            repo.get_recording_release_groups_batch(["rec-1"])
        )
        second = asyncio.create_task(
            repo.get_recording_release_groups_batch(["rec-1"])
        )
        await request_started.wait()
        await asyncio.sleep(0)
        allow_response.set()

        results = await asyncio.gather(first, second)

        assert results == [{"rec-1": "rg-1"}, {"rec-1": "rg-1"}]
        repo._post.assert_awaited_once()
