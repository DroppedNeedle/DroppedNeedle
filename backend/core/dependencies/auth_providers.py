"""Auth dependency providers, wired into the singleton DI system."""

from __future__ import annotations

from core.config import get_settings
from infrastructure.persistence.auth_store import AuthStore

from ._registry import singleton
from .cache_providers import (
    get_discovery_snapshot_store,
    get_persistence_write_lock,
    get_preferences_service,
)


@singleton
def get_auth_store() -> AuthStore:
    settings = get_settings()
    lock = get_persistence_write_lock()
    return AuthStore(db_path = settings.library_db_path, write_lock = lock)


@singleton
def get_auth_service() -> "AuthService":
    from services.auth_service import AuthService
    return AuthService(
        get_auth_store(), discovery_snapshot_store=get_discovery_snapshot_store()
    )


@singleton
def get_plex_user_auth_service() -> "PlexUserAuthService":
    from services.plex_user_auth_service import PlexUserAuthService
    from core.dependencies.repo_providers import get_plex_repository, get_user_connections_store
    return PlexUserAuthService(
        auth_store = get_auth_store(),
        plex_repository = get_plex_repository(),
        preferences_service = get_preferences_service(),
        connections_store = get_user_connections_store(),
    )


@singleton
def get_jellyfin_user_auth_service() -> "JellyfinUserAuthService":
    from services.jellyfin_user_auth_service import JellyfinUserAuthService
    from core.dependencies.repo_providers import get_jellyfin_repository, get_user_connections_store
    return JellyfinUserAuthService(
        auth_store = get_auth_store(),
        jellyfin_repository = get_jellyfin_repository(),
        preferences_service = get_preferences_service(),
        connections_store = get_user_connections_store(),
    )


@singleton
def get_user_import_service() -> "UserImportService":
    from services.user_import_service import UserImportService
    from core.dependencies.repo_providers import (
        get_jellyfin_repository,
        get_plex_repository,
    )
    return UserImportService(
        auth_store = get_auth_store(),
        jellyfin_repository = get_jellyfin_repository(),
        plex_repository = get_plex_repository(),
        preferences_service = get_preferences_service(),
    )


@singleton
def get_oidc_user_auth_service() -> "OIDCUserAuthService":
    from services.oidc_user_auth_service import OIDCUserAuthService
    from core.dependencies.cache_providers import get_cache
    return OIDCUserAuthService(
        auth_store = get_auth_store(),
        preferences_service = get_preferences_service(),
        cache = get_cache(),
    )
