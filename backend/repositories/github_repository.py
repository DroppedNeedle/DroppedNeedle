import logging
import os

import httpx
import msgspec

from api.v1.schemas.version import GitHubRelease
from infrastructure.cache.cache_keys import GITHUB_RELEASES_PREFIX
from infrastructure.cache.memory_cache import CacheInterface

logger = logging.getLogger(__name__)

DEFAULT_RELEASES_REPOSITORY = "DroppedNeedle/DroppedNeedle"
RELEASES_REPOSITORY_ENV = "DROPPEDNEEDLE_RELEASES_REPOSITORY"
GITHUB_RELEASES_CACHE_TTL = 3600


class _GitHubReleaseRaw(msgspec.Struct):
    """Raw GitHub API response struct for decoding."""

    tag_name: str
    published_at: str
    html_url: str
    name: str | None = None
    body: str | None = None
    prerelease: bool = False
    draft: bool = False


class GitHubRepository:
    def __init__(self, http_client: httpx.AsyncClient, cache: CacheInterface):
        self._client = http_client
        self._cache = cache
        configured_repository = os.environ.get(RELEASES_REPOSITORY_ENV)
        self._releases_repository = (
            configured_repository or DEFAULT_RELEASES_REPOSITORY
        ).strip("/")
        self._api_url = (
            f"https://api.github.com/repos/{self._releases_repository}/releases"
        )
        self._cache_key = (
            f"{GITHUB_RELEASES_PREFIX}{self._releases_repository.lower()}:all"
        )

    async def fetch_releases(self) -> list[GitHubRelease]:
        """Fetch all non-draft releases from GitHub, with 1hr server-side cache."""
        cached = await self._cache.get(self._cache_key)
        if cached is not None:
            return cached

        try:
            response = await self._client.get(
                self._api_url,
                headers={"Accept": "application/vnd.github+json"},
                timeout=10.0,
            )
            if response.status_code != 200:
                logger.warning("GitHub releases API returned %s", response.status_code)
                return []

            raw_releases = msgspec.json.decode(
                response.content, type=list[_GitHubReleaseRaw]
            )
            releases = [
                GitHubRelease(
                    tag_name=r.tag_name,
                    name=r.name or r.tag_name,
                    body=r.body or "",
                    published_at=r.published_at,
                    html_url=r.html_url,
                    prerelease=r.prerelease,
                )
                for r in raw_releases
                if not r.draft
            ]

            await self._cache.set(
                self._cache_key,
                releases,
                ttl_seconds=GITHUB_RELEASES_CACHE_TTL,
            )
            return releases

        except (httpx.HTTPError, msgspec.DecodeError) as e:
            logger.error("Failed to fetch GitHub releases: %s", e)
            return []

    async def fetch_latest_release(self) -> GitHubRelease | None:
        """Get the latest non-prerelease release."""
        releases = await self.fetch_releases()
        for release in releases:
            if not release.prerelease:
                return release
        return None
