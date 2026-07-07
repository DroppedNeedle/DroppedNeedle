import logging

import httpx
import msgspec

from api.v1.schemas.version import GitHubRelease
from infrastructure.cache.cache_keys import GITHUB_RELEASES_PREFIX
from infrastructure.cache.memory_cache import CacheInterface

logger = logging.getLogger(__name__)

GITHUB_API_URL = "https://api.github.com/repos/HabiRabbu/DroppedNeedle/releases"
GITHUB_RELEASES_CACHE_KEY = f"{GITHUB_RELEASES_PREFIX}all"
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

    async def fetch_releases(self) -> list[GitHubRelease]:
        """Fetch all non-draft releases from GitHub, with 1hr server-side cache."""
        cached = await self._cache.get(GITHUB_RELEASES_CACHE_KEY)
        if cached is not None:
            return cached

        try:
            response = await self._client.get(
                GITHUB_API_URL,
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
                GITHUB_RELEASES_CACHE_KEY,
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
