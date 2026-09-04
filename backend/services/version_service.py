import logging
import os

from packaging.version import InvalidVersion, Version

from api.v1.schemas.version import GitHubRelease, UpdateCheckResponse, VersionInfo
from repositories.github_repository import GitHubRepository

logger = logging.getLogger(__name__)


class VersionService:
    def __init__(self, github_repo: GitHubRepository):
        self._github_repo = github_repo

    def get_current_version(self) -> VersionInfo:
        version = os.environ.get("COMMIT_TAG", "dev")
        build_date = os.environ.get("BUILD_DATE")
        return VersionInfo(version=version, build_date=build_date)

    async def check_for_updates(self) -> UpdateCheckResponse:
        current = self.get_current_version()
        latest = await self._github_repo.fetch_latest_release()

        if latest is None:
            return UpdateCheckResponse(current_version=current.version)

        update_available, comparison_failed = self._is_newer(
            latest.tag_name, current.version
        )

        # Dev builds: simulate update available so the full UI can be tested
        is_dev = current.version in ("dev", "hosting-local")
        if comparison_failed and is_dev:
            update_available = True

        return UpdateCheckResponse(
            current_version=current.version,
            latest_version=latest.tag_name,
            update_available=update_available,
            comparison_failed=comparison_failed,
            latest_release=latest if update_available else None,
        )

    async def get_release_history(self) -> list[GitHubRelease]:
        return await self._github_repo.fetch_releases()

    @staticmethod
    def _is_newer(latest_tag: str, current_tag: str) -> tuple[bool, bool]:
        """Compare version tags. Returns (update_available, comparison_failed)."""
        try:
            latest = Version(latest_tag.lstrip("v"))
            current = Version(current_tag.lstrip("v"))
            return (latest > current, False)
        except InvalidVersion:
            logger.warning(
                "Invalid version comparison: %s vs %s", latest_tag, current_tag
            )
            return (False, True)
