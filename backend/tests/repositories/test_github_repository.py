from unittest.mock import AsyncMock

import httpx
import pytest

from repositories.github_repository import GitHubRepository


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("repository", "expected_url", "expected_cache_key"),
    [
        (
            None,
            "https://api.github.com/repos/DroppedNeedle/DroppedNeedle/releases",
            "github:releases:droppedneedle/droppedneedle:all",
        ),
        (
            "",
            "https://api.github.com/repos/DroppedNeedle/DroppedNeedle/releases",
            "github:releases:droppedneedle/droppedneedle:all",
        ),
        (
            "alphyriver/DroppedNeedle",
            "https://api.github.com/repos/alphyriver/DroppedNeedle/releases",
            "github:releases:alphyriver/droppedneedle:all",
        ),
    ],
)
async def test_release_repository_controls_api_and_cache_namespace(
    monkeypatch: pytest.MonkeyPatch,
    repository: str | None,
    expected_url: str,
    expected_cache_key: str,
):
    if repository is None:
        monkeypatch.delenv("DROPPEDNEEDLE_RELEASES_REPOSITORY", raising=False)
    else:
        monkeypatch.setenv("DROPPEDNEEDLE_RELEASES_REPOSITORY", repository)

    cache = AsyncMock()
    cache.get.return_value = None
    client = AsyncMock()
    client.get.return_value = httpx.Response(200, json=[])

    releases = await GitHubRepository(client, cache).fetch_releases()

    assert releases == []
    cache.get.assert_awaited_once_with(expected_cache_key)
    cache.set.assert_awaited_once_with(expected_cache_key, [], ttl_seconds=3600)
    assert client.get.await_args.args[0] == expected_url
