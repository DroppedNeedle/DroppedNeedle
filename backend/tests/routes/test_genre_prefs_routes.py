import threading

import pytest
from unittest.mock import AsyncMock

from fastapi import FastAPI

import api.v1.routes.me_connections as me_connections
from api.v1.routes.me_connections import router
from core.dependencies import (
    get_genre_index,
    get_user_genre_prefs_store,
)
from infrastructure.persistence.user_genre_prefs_store import UserGenrePrefsStore
from tests.helpers import build_test_client, override_user_auth

_UID = "test-user-id"


@pytest.fixture
def store(tmp_path):
    return UserGenrePrefsStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())


@pytest.fixture
def genre_index():
    index = AsyncMock()
    # three spellings of the K-pop family plus two distinct families
    index.get_top_genres = AsyncMock(
        return_value=[("k-pop", 30), ("kpop", 8), ("korean pop", 4), ("rock", 12), ("jazz", 3)]
    )
    return index


@pytest.fixture
def client(store, genre_index, monkeypatch):
    # cache invalidation reaches into app singletons; not under test here
    monkeypatch.setattr(
        me_connections, "_invalidate_recommendation_caches", AsyncMock()
    )
    app = FastAPI()
    app.include_router(router)
    app.dependency_overrides[get_user_genre_prefs_store] = lambda: store
    app.dependency_overrides[get_genre_index] = lambda: genre_index
    override_user_auth(app, user_id=_UID)
    return build_test_client(app)


class TestGetGenrePrefs:
    def test_families_merge_spellings_and_sum_counts(self, client):
        resp = client.get("/me/genre-prefs")
        assert resp.status_code == 200
        genres = {g["family"]: g for g in resp.json()["genres"]}
        # "k-pop" + "kpop" + "korean pop" merge into one family
        assert set(genres) == {"k-pop", "rock", "jazz"}
        assert genres["k-pop"]["artist_count"] == 42
        assert all(g["level"] == "normal" for g in genres.values())


class TestPutGenrePrefs:
    def test_reduce_and_mute_round_trip(self, client):
        resp = client.put(
            "/me/genre-prefs",
            json={
                "genres": [
                    # any spelling normalises to the family key
                    {"family": "kpop", "level": "mute"},
                    {"family": "rock", "level": "reduce"},
                    {"family": "jazz", "level": "normal"},
                ]
            },
        )
        assert resp.status_code == 200
        genres = {g["family"]: g["level"] for g in resp.json()["genres"]}
        assert genres["k-pop"] == "mute"
        assert genres["rock"] == "reduce"
        assert genres["jazz"] == "normal"

        # back to normal clears the stored rows
        resp = client.put(
            "/me/genre-prefs",
            json={"genres": [{"family": "k-pop", "level": "normal"}]},
        )
        genres = {g["family"]: g["level"] for g in resp.json()["genres"]}
        assert all(level == "normal" for level in genres.values())

    def test_invalid_level_rejected(self, client):
        resp = client.put(
            "/me/genre-prefs",
            json={"genres": [{"family": "rock", "level": "banish"}]},
        )
        assert resp.status_code == 422

    def test_stored_family_missing_from_library_still_listed(self, client):
        client.put(
            "/me/genre-prefs",
            json={"genres": [{"family": "vaporwave", "level": "mute"}]},
        )
        resp = client.get("/me/genre-prefs")
        genres = {g["family"]: g for g in resp.json()["genres"]}
        assert genres["vaporwave"]["level"] == "mute"
        assert genres["vaporwave"]["artist_count"] == 0
