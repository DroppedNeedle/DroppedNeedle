import asyncio
import logging
import uuid
import msgspec

from api.v1.schemas.settings import (
    JellyfinConnectionSettings,
    ListenBrainzConnectionSettings,
    NavidromeConnectionSettings,
    YouTubeConnectionSettings,
    LastFmConnectionSettings,
    NAVIDROME_PASSWORD_MASK,
    LASTFM_SECRET_MASK,
    PlexConnectionSettings,
    PLEX_TOKEN_MASK,
    DownloadClientConnectionSettings,
    DOWNLOAD_CLIENT_API_KEY_MASK,
    MusicBrainzConnectionSettings,
    MusicBrainzSettingsUpdate,
    MusicBrainzBindingRequest,
    BRAINZMASH_ENDPOINT,
    BRAINZMASH_DISCLOSURE_VERSION,
)
from core.config import get_settings
from core.exceptions import ConfigurationError, RateLimitedError, ValidationError
from models.common import ServiceStatus
from infrastructure.cache.cache_keys import (
    JELLYFIN_PREFIX,
    LOCAL_FILES_PREFIX,
    SOURCE_RESOLUTION_PREFIX,
    musicbrainz_prefixes,
    listenbrainz_prefixes,
    lastfm_prefixes,
    home_prefixes,
)
from infrastructure.cache.memory_cache import InMemoryCache, CacheInterface
from infrastructure.http.client import get_http_client
from repositories.jellyfin_models import JellyfinUser
from models.release_type_policy import normalize_release_type_filters

logger = logging.getLogger(__name__)


class JellyfinVerifyResult(msgspec.Struct):
    success: bool
    message: str
    users: list[JellyfinUser] | None = None


class ListenBrainzVerifyResult(msgspec.Struct):
    valid: bool
    message: str


class NavidromeVerifyResult(msgspec.Struct):
    valid: bool
    message: str


class PlexVerifyResult(msgspec.Struct):
    valid: bool
    message: str
    libraries: list[tuple[str, str]] = []


class YouTubeVerifyResult(msgspec.Struct):
    valid: bool
    message: str


class LastFmVerifyResult(msgspec.Struct):
    valid: bool
    message: str


class MusicBrainzVerifyResult(msgspec.Struct):
    valid: bool
    message: str


class SettingsService:
    def __init__(
        self,
        preferences_service,
        cache: CacheInterface,
        *,
        navidrome_library_getter=None,
        plex_library_getter=None,
        discovery_snapshot_store=None,
        disk_cache=None,
    ):
        self._preferences_service = preferences_service
        self._cache = cache
        self._navidrome_library_getter = navidrome_library_getter
        self._plex_library_getter = plex_library_getter
        self._discovery_snapshot_store = discovery_snapshot_store
        self._disk_cache = disk_cache
        self._musicbrainz_coordinator_lock = asyncio.Lock()

    async def verify_jellyfin(
        self, settings: JellyfinConnectionSettings
    ) -> JellyfinVerifyResult:
        try:
            from infrastructure.validators import validate_service_url

            validate_service_url(settings.jellyfin_url, label="Jellyfin URL")

            from repositories.jellyfin_repository import JellyfinRepository

            JellyfinRepository.reset_circuit_breaker()

            app_settings = get_settings()
            http_client = get_http_client(app_settings)
            temp_cache = InMemoryCache(max_entries=100)

            temp_repo = JellyfinRepository(http_client=http_client, cache=temp_cache)
            temp_repo.configure(
                base_url=settings.jellyfin_url,
                api_key=settings.api_key,
                user_id=settings.user_id,
            )

            success, message = await temp_repo.validate_connection()

            users = []
            if success:
                jf_users = await temp_repo.fetch_users_direct()
                users = [JellyfinUser(id=u.id, name=u.name) for u in jf_users]

            return JellyfinVerifyResult(success=success, message=message, users=users)
        except Exception as e:  # noqa: BLE001
            logger.exception(f"Failed to verify Jellyfin connection: {e}")
            return JellyfinVerifyResult(
                success=False, message="Couldn't finish the connection test"
            )

    async def verify_listenbrainz(
        self, settings: ListenBrainzConnectionSettings
    ) -> ListenBrainzVerifyResult:
        try:
            from repositories.listenbrainz_repository import ListenBrainzRepository

            app_settings = get_settings()
            http_client = get_http_client(app_settings)
            temp_cache = InMemoryCache(max_entries=100)

            temp_repo = ListenBrainzRepository(
                http_client=http_client, cache=temp_cache
            )
            temp_repo.configure(
                username=settings.username, user_token=settings.user_token
            )

            if settings.user_token:
                valid, message = await temp_repo.validate_token()
            else:
                valid, message = await temp_repo.validate_username(settings.username)

            return ListenBrainzVerifyResult(valid=valid, message=message)
        except RateLimitedError:
            raise
        except Exception as e:  # noqa: BLE001
            logger.exception(f"Failed to verify ListenBrainz connection: {e}")
            return ListenBrainzVerifyResult(
                valid=False, message="Couldn't finish the connection test"
            )

    @staticmethod
    def _type_filters(prefs) -> tuple[frozenset[str], frozenset[str]]:
        return normalize_release_type_filters(
            prefs.primary_types,
            prefs.secondary_types,
        )

    async def apply_preference_change(self, previous, incoming) -> int:
        """ST1 phase 1 diff gate for PUT /settings/preferences.

        - Identical payload (same normalized type filters): NO sweep at all -
          a no-change save no longer destroys artist/album/MB caches.
        - Changed types: ZERO prefix sweeps. Raw MB caches apply filters at
          request time from live preferences (artist_service), and the one
          proven baked consumer (SearchService.search) now embeds the sorted
          type sets in its cache key, so stale results are unreachable
          immediately; the in-process search cache is still flushed as
          belt-and-braces for the transition window.

        Returns the number of cleared entries (always 0 in phase 1; kept for
        call-site/logging compatibility).
        """
        if previous is not None and self._type_filters(previous) == self._type_filters(
            incoming
        ):
            logger.info(
                "Preferences payload unchanged; skipping catalog cache invalidation"
            )
            return 0

        from services.search_service import SearchService

        SearchService.clear_cached_results()
        logger.info("Preference types changed; search cache flushed (no prefix sweeps)")
        return 0

    async def clear_home_cache(self) -> int:
        total = 0
        for prefix in home_prefixes():
            total += await self._cache.clear_prefix(prefix)
        total += await self._cache.clear_prefix(JELLYFIN_PREFIX)
        for prefix in listenbrainz_prefixes():
            total += await self._cache.clear_prefix(prefix)
        for prefix in lastfm_prefixes():
            total += await self._cache.clear_prefix(prefix)
        if self._discovery_snapshot_store is not None:
            await self._discovery_snapshot_store.mark_discover_stale()
        logger.info(f"Cleared {total} home/discover/integration cache entries")
        return total

    async def clear_local_files_cache(self) -> int:
        cleared = await self._cache.clear_prefix(LOCAL_FILES_PREFIX)
        logger.info(f"Cleared {cleared} local files cache entries")
        return cleared

    async def clear_source_resolution_cache(self) -> int:
        cleared = await self._cache.clear_prefix(SOURCE_RESOLUTION_PREFIX)
        logger.info(f"Cleared {cleared} source-resolution cache entries")
        return cleared

    async def on_jellyfin_settings_changed(self) -> None:
        from repositories.jellyfin_repository import JellyfinRepository
        from core.dependencies import (
            get_jellyfin_repository,
            get_jellyfin_playback_service,
            get_jellyfin_library_service,
            get_home_service,
            get_home_charts_service,
            get_mbid_store,
            get_target_coverart_repository,
            get_target_consumer_composition,
            get_target_compat_services,
            get_target_discover_queue_manager,
            get_target_discover_service,
            get_target_genre_cover_prewarm_service,
            get_target_home_charts_service,
            get_target_home_service,
            get_target_search_service,
            get_target_wrapped_service,
        )
        from core.dependencies.auth_providers import (
            get_user_import_service,
            get_jellyfin_user_auth_service,
        )

        JellyfinRepository.reset_circuit_breaker()
        get_jellyfin_repository.cache_clear()
        get_jellyfin_playback_service.cache_clear()
        get_jellyfin_library_service.cache_clear()
        get_home_service.cache_clear()
        get_home_charts_service.cache_clear()
        get_target_coverart_repository.cache_clear()
        get_target_consumer_composition.cache_clear()
        get_target_compat_services.cache_clear()
        get_target_search_service.cache_clear()
        get_target_genre_cover_prewarm_service.cache_clear()
        get_target_home_service.cache_clear()
        get_target_home_charts_service.cache_clear()
        get_target_wrapped_service.cache_clear()
        get_target_discover_service.cache_clear()
        get_target_discover_queue_manager.cache_clear()
        # The import + SSO-login services capture the jellyfin repo singleton;
        # rebuild them so a newly-configured Jellyfin is enumerable and usable for
        # login without an app restart.
        get_user_import_service.cache_clear()
        get_jellyfin_user_auth_service.cache_clear()
        mbid_store = get_mbid_store()
        await mbid_store.clear_jellyfin_mbid_index()
        await self.clear_home_cache()
        await self.clear_source_resolution_cache()
        logger.info("Jellyfin settings change: all caches/singletons reset")

    async def on_navidrome_settings_changed(self, enabled: bool = False) -> None:
        from repositories.navidrome_repository import NavidromeRepository
        from core.dependencies import (
            get_navidrome_repository,
            get_navidrome_library_service,
            get_target_navidrome_library_service,
            get_navidrome_folder_scope_service,
            get_navidrome_playback_service,
            get_library_service,
            get_home_service,
            get_home_charts_service,
            get_mbid_store,
        )

        NavidromeRepository.reset_circuit_breaker()
        get_navidrome_repository.cache_clear()
        get_navidrome_library_service.cache_clear()
        get_target_navidrome_library_service.cache_clear()
        get_navidrome_folder_scope_service.cache_clear()
        get_navidrome_playback_service.cache_clear()
        get_library_service.cache_clear()
        get_home_service.cache_clear()
        get_home_charts_service.cache_clear()
        mbid_store = get_mbid_store()
        await mbid_store.clear_navidrome_mbid_indexes()
        new_repo = get_navidrome_repository()
        await new_repo.clear_cache()
        await self.clear_home_cache()
        await self.clear_source_resolution_cache()
        if enabled:
            import asyncio
            from core.tasks import warm_navidrome_mbid_cache
            from core.task_registry import TaskRegistry

            registry = TaskRegistry.get_instance()
            if not registry.is_running("navidrome-mbid-warmup"):
                _nav_task = asyncio.create_task(
                    warm_navidrome_mbid_cache(self._navidrome_library_getter)
                )
                try:
                    registry.register("navidrome-mbid-warmup", _nav_task)
                except RuntimeError:
                    pass
        logger.info("Navidrome settings change: all caches/singletons reset")

    async def on_lastfm_settings_changed(self) -> None:
        from repositories.lastfm_repository import LastFmRepository
        from core.dependencies import (
            get_lastfm_repository,
            get_lastfm_auth_service,
            clear_lastfm_dependent_caches,
        )

        LastFmRepository.reset_circuit_breaker()
        get_lastfm_repository.cache_clear()
        get_lastfm_auth_service.cache_clear()
        clear_lastfm_dependent_caches()
        await self.clear_home_cache()
        logger.info("Last.fm settings change: all caches/singletons reset")

    async def on_listenbrainz_settings_changed(self) -> None:
        from repositories.listenbrainz_repository import ListenBrainzRepository
        from core.dependencies import clear_listenbrainz_dependent_caches

        ListenBrainzRepository.reset_circuit_breaker()
        clear_listenbrainz_dependent_caches()
        await self.clear_home_cache()
        logger.info("ListenBrainz settings change: all caches/singletons reset")

    async def on_listenbrainz_connection_changed(self) -> None:
        """Invalidate shared ListenBrainz state after a per-user mutation.

        This deliberately resets only the circuit breaker.  The process-global
        rate-limit response window and cooldown remain intact so a user changing
        credentials cannot bypass upstream pacing for other requests.
        """
        from repositories.listenbrainz_repository import ListenBrainzRepository
        from core.dependencies import clear_listenbrainz_dependent_caches

        ListenBrainzRepository.reset_circuit_breaker()
        clear_listenbrainz_dependent_caches()
        await self.clear_home_cache()
        logger.info("ListenBrainz user connection changed: caches/singletons reset")

    async def on_youtube_settings_changed(self) -> None:
        from core.dependencies import get_youtube_repo

        get_youtube_repo.cache_clear()
        await self.clear_home_cache()
        logger.info("YouTube settings change: singleton reset, home caches cleared")

    async def on_http_settings_changed(self) -> None:
        """F-PERF-08: advanced HTTP timeout/pool values changed.

        Retire the shared factory generations so the next resolution builds
        clients from the saved settings, clear the provider graphs that hold
        default/ListenBrainz/cover-art clients, and close the superseded
        generations through the awaited lifecycle path."""
        from infrastructure.http.client import HttpClientFactory
        from repositories.musicbrainz_base import set_mb_brainzmash_http_client

        for logical_name in (
            "default",
            "listenbrainz",
            "coverart",
            "musicbrainz-brainzmash",
        ):
            HttpClientFactory.retire_name(logical_name)
        set_mb_brainzmash_http_client(None)
        from core.dependencies import clear_listenbrainz_dependent_caches

        clear_listenbrainz_dependent_caches()
        await self.on_coverart_settings_changed()
        await HttpClientFactory.close_retired()

    async def on_coverart_settings_changed(self) -> None:
        from core.dependencies import (
            get_coverart_repository,
            clear_library_management_provider_graph,
            get_target_consumer_composition,
            get_target_compat_services,
            get_target_coverart_repository,
            get_target_discover_queue_manager,
            get_target_discover_service,
            get_target_genre_cover_prewarm_service,
            get_target_home_charts_service,
            get_target_search_service,
            get_target_wrapped_service,
        )

        get_coverart_repository.cache_clear()
        get_target_coverart_repository.cache_clear()
        get_target_consumer_composition.cache_clear()
        get_target_compat_services.cache_clear()
        get_target_search_service.cache_clear()
        get_target_genre_cover_prewarm_service.cache_clear()
        get_target_home_charts_service.cache_clear()
        get_target_wrapped_service.cache_clear()
        get_target_discover_service.cache_clear()
        get_target_discover_queue_manager.cache_clear()
        clear_library_management_provider_graph()
        logger.info("Coverart settings change: singleton reset")

    async def verify_navidrome(
        self, settings: NavidromeConnectionSettings
    ) -> NavidromeVerifyResult:
        try:
            from infrastructure.validators import validate_service_url

            validate_service_url(settings.navidrome_url, label="Navidrome URL")

            from repositories.navidrome_repository import NavidromeRepository

            NavidromeRepository.reset_circuit_breaker()

            app_settings = get_settings()
            http_client = get_http_client(app_settings)
            temp_cache = InMemoryCache(max_entries=100)

            temp_repo = NavidromeRepository(http_client=http_client, cache=temp_cache)

            password = settings.password
            if password == NAVIDROME_PASSWORD_MASK:
                raw = self._preferences_service.get_navidrome_connection_raw()
                password = raw.password

            temp_repo.configure(
                url=settings.navidrome_url,
                username=settings.username,
                password=password,
            )

            ok = await temp_repo.ping()
            if ok:
                return NavidromeVerifyResult(
                    valid=True, message="Connected to Navidrome successfully"
                )
            return NavidromeVerifyResult(
                valid=False,
                message="Navidrome didn't respond. Check the URL and credentials.",
            )
        except Exception as e:  # noqa: BLE001
            logger.exception("Failed to verify Navidrome connection: %s", e)
            return NavidromeVerifyResult(
                valid=False,
                message="Couldn't finish the connection test",
            )

    async def verify_youtube(
        self, settings: YouTubeConnectionSettings
    ) -> YouTubeVerifyResult:
        try:
            from repositories.youtube import YouTubeRepository

            app_settings = get_settings()
            http_client = get_http_client(app_settings)
            temp_repo = YouTubeRepository(
                http_client=http_client,
                api_key=settings.api_key.strip(),
                daily_quota_limit=settings.daily_quota_limit,
            )
            valid, message = await temp_repo.verify_api_key(settings.api_key.strip())
            return YouTubeVerifyResult(valid=valid, message=message)
        except Exception as e:  # noqa: BLE001
            logger.exception("Failed to verify YouTube connection: %s", e)
            return YouTubeVerifyResult(
                valid=False,
                message="Couldn't finish the connection test",
            )

    async def verify_lastfm(
        self, settings: LastFmConnectionSettings
    ) -> LastFmVerifyResult:
        try:
            from repositories.lastfm_repository import LastFmRepository

            app_settings = get_settings()
            http_client = get_http_client(app_settings)

            current = self._preferences_service.get_lastfm_connection()
            shared_secret = settings.shared_secret
            if shared_secret.startswith(LASTFM_SECRET_MASK):
                shared_secret = current.shared_secret

            session_key = settings.session_key
            if session_key.startswith(LASTFM_SECRET_MASK):
                session_key = current.session_key

            temp_repo = LastFmRepository(
                http_client=http_client,
                cache=InMemoryCache(),
                api_key=settings.api_key,
                shared_secret=shared_secret,
                session_key=session_key,
            )
            valid, message = await temp_repo.validate_api_key()
            if not valid:
                return LastFmVerifyResult(valid=False, message=message)

            if session_key:
                session_valid, session_message = await temp_repo.validate_session()
                if not session_valid:
                    return LastFmVerifyResult(
                        valid=False,
                        message=f"The API key looks good, but the saved session isn't valid: {session_message}",
                    )
                return LastFmVerifyResult(valid=True, message=session_message)

            return LastFmVerifyResult(valid=valid, message=message)
        except Exception as e:  # noqa: BLE001
            logger.exception("Failed to verify Last.fm connection: %s", e)
            return LastFmVerifyResult(
                valid=False, message="Couldn't finish the Last.fm connection test"
            )

    async def verify_plex(self, settings: PlexConnectionSettings) -> PlexVerifyResult:
        try:
            from infrastructure.validators import validate_service_url

            validate_service_url(settings.plex_url, label="Plex URL")

            from repositories.plex_repository import PlexRepository

            PlexRepository.reset_circuit_breaker()

            app_settings = get_settings()
            http_client = get_http_client(app_settings)
            temp_cache = InMemoryCache(max_entries=100)

            token = settings.plex_token
            if token == PLEX_TOKEN_MASK:
                raw = self._preferences_service.get_plex_connection_raw()
                token = raw.plex_token

            client_id = self._preferences_service.get_setting("plex_client_id") or ""

            temp_repo = PlexRepository(http_client=http_client, cache=temp_cache)
            temp_repo.configure(
                url=settings.plex_url,
                token=token,
                client_id=client_id,
            )

            ok, message = await temp_repo.validate_connection()
            libs: list[tuple[str, str]] = []
            if ok:
                try:
                    sections = await temp_repo.get_music_libraries()
                    libs = [(s.key, s.title) for s in sections]
                except Exception:  # noqa: BLE001
                    logger.warning("Plex verify succeeded but library fetch failed")
            return PlexVerifyResult(valid=ok, message=message, libraries=libs)
        except Exception as e:  # noqa: BLE001
            logger.exception("Failed to verify Plex connection: %s", e)
            return PlexVerifyResult(
                valid=False,
                message="Couldn't finish the Plex connection test",
            )

    async def verify_download_client(
        self, settings: DownloadClientConnectionSettings
    ) -> ServiceStatus:
        """Health-check the submitted slskd url/key without saving, so Test-connection
        validates the form (stored-config test fails before the first save). A masked
        api_key falls back to the stored secret, mirroring verify_plex."""
        try:
            from infrastructure.validators import validate_service_url

            validate_service_url(settings.url, label="Download client URL")

            api_key = settings.api_key
            if api_key == DOWNLOAD_CLIENT_API_KEY_MASK:
                api_key = (
                    self._preferences_service.get_download_client_settings_raw().api_key
                )

            from core.dependencies import build_slskd_repository

            repo = build_slskd_repository(settings.url, api_key)
            return await repo.health_check()
        except ValidationError as e:
            return ServiceStatus(status="error", message=str(e))
        except Exception as e:  # noqa: BLE001
            logger.exception("Failed to verify download client connection: %s", e)
            return ServiceStatus(
                status="error", message="Couldn't finish the connection test"
            )

    async def on_plex_settings_changed(self, enabled: bool = False) -> None:
        from repositories.plex_repository import PlexRepository
        from core.dependencies import (
            get_plex_repository,
            get_plex_library_service,
            get_target_plex_library_service,
            get_plex_playback_service,
            get_home_service,
            get_home_charts_service,
            get_mbid_store,
        )
        from core.dependencies.auth_providers import (
            get_user_import_service,
            get_plex_user_auth_service,
        )

        PlexRepository.reset_circuit_breaker()
        get_plex_repository.cache_clear()
        get_plex_library_service.cache_clear()
        get_target_plex_library_service.cache_clear()
        get_plex_playback_service.cache_clear()
        get_home_service.cache_clear()
        get_home_charts_service.cache_clear()
        # The import + SSO-login services capture the plex repo singleton; rebuild
        # them so a newly-configured Plex is enumerable and usable for login
        # without an app restart.
        get_user_import_service.cache_clear()
        get_plex_user_auth_service.cache_clear()
        mbid_store = get_mbid_store()
        await mbid_store.clear_plex_mbid_indexes()
        new_repo = get_plex_repository()
        await new_repo.clear_cache()
        await self.clear_home_cache()
        await self.clear_source_resolution_cache()
        if enabled:
            import asyncio
            from core.tasks import warm_plex_mbid_cache
            from core.task_registry import TaskRegistry

            registry = TaskRegistry.get_instance()
            if not registry.is_running("plex-mbid-warmup"):
                _plex_task = asyncio.create_task(
                    warm_plex_mbid_cache(self._plex_library_getter)
                )
                try:
                    registry.register("plex-mbid-warmup", _plex_task)
                except RuntimeError:
                    pass
        logger.info("Plex settings change: all caches/singletons reset")

    async def get_plex_libraries(self) -> list[tuple[str, str]]:
        raw = self._preferences_service.get_plex_connection_raw()
        if not raw.plex_url or not raw.plex_token:
            raise ValueError("Plex is not configured")

        from repositories.plex_repository import PlexRepository

        app_settings = get_settings()
        http_client = get_http_client(app_settings)
        temp_cache = InMemoryCache(max_entries=100)
        client_id = self._preferences_service.get_setting("plex_client_id") or ""
        temp_repo = PlexRepository(http_client=http_client, cache=temp_cache)
        temp_repo.configure(url=raw.plex_url, token=raw.plex_token, client_id=client_id)
        sections = await temp_repo.get_music_libraries()
        return [(s.key, s.title) for s in sections]

    async def verify_brainzmash(
        self, binding: MusicBrainzBindingRequest
    ) -> MusicBrainzVerifyResult:
        """Verify the server-owned, consent-bound BrainzMash proposal."""
        current = self._preferences_service.get_musicbrainz_connection()
        pending = current.pending_brainzmash
        if (
            pending is None
            or pending.access_revision != binding.access_revision
            or pending.source_id != binding.source_id
            or pending.generation != binding.generation
            or pending.disclosure_version != binding.disclosure_version
        ):
            raise ValidationError("BrainzMash proposal is stale")
        if not pending.consented:
            raise ValidationError("BrainzMash consent is required")
        if pending.disclosure_version != BRAINZMASH_DISCLOSURE_VERSION:
            raise ValidationError("BrainzMash disclosure is outdated")
        from infrastructure.http.brainzmash_transport import validate_brainzmash_url
        from repositories.musicbrainz_base import (
            MbSourceContext,
            capture_mb_source_context,
        )

        admission_context = capture_mb_source_context()

        def pending_is_current() -> bool:
            latest = self._preferences_service.get_musicbrainz_connection()
            candidate = latest.pending_brainzmash
            return bool(
                candidate is not None
                and self._preferences_service._pending_brainzmash_policy_is_current(
                    candidate
                )
                and candidate.access_revision == binding.access_revision
                and candidate.source_id == binding.source_id
                and candidate.generation == binding.generation
                and candidate.disclosure_version == binding.disclosure_version
                and candidate.consented
            )

        try:
            validate_brainzmash_url(pending.endpoint)
        except ValueError as exc:
            raise ValidationError("BrainzMash endpoint is outdated") from exc
        try:
            from repositories.musicbrainz_base import mb_api_probe
            from infrastructure.http.client import get_brainzmash_http_client

            response = await mb_api_probe(
                BRAINZMASH_ENDPOINT,
                client=get_brainzmash_http_client(),
                allow_unbound_brainzmash=True,
                brainzmash=True,
                source_context=MbSourceContext(
                    source_url=BRAINZMASH_ENDPOINT.rstrip("/"),
                    generation=pending.generation,
                    source_mode="brainzmash",
                    source_id=pending.source_id,
                ),
                admission_context=admission_context,
                admission_check=pending_is_current,
            )
        except ValidationError:
            raise
        except Exception as exc:  # noqa: BLE001 - route returns a safe provider error
            logger.warning("BrainzMash verification failed: %s", type(exc).__name__)
            return MusicBrainzVerifyResult(
                valid=False, message="Could not connect to BrainzMash"
            )
        if response.status_code != 200:
            return MusicBrainzVerifyResult(
                valid=False,
                message=f"BrainzMash verification returned HTTP {response.status_code}",
            )
        return MusicBrainzVerifyResult(valid=True, message="Connected to BrainzMash")

    async def verify_musicbrainz(
        self, settings: MusicBrainzConnectionSettings
    ) -> MusicBrainzVerifyResult:
        from api.v1.schemas.settings import is_brainzmash_active_binding_valid
        from repositories.musicbrainz_base import (
            MbSourceContext,
            brainzmash_runtime_enabled,
            capture_mb_source_context,
            mb_api_probe,
        )

        if settings.source_mode == "brainzmash":
            return MusicBrainzVerifyResult(
                valid=False,
                message="Use the consent-bound BrainzMash verification flow",
            )

        current = self._preferences_service.get_musicbrainz_connection()
        admission_context = capture_mb_source_context()
        admission_revision = (
            self._preferences_service.get_musicbrainz_settings_revision()
        )
        active_brainzmash = (
            brainzmash_runtime_enabled() and is_brainzmash_active_binding_valid(current)
        )
        if active_brainzmash and settings.source_mode != "brainzmash":
            return MusicBrainzVerifyResult(
                valid=False,
                message="Alternative MusicBrainz tests are disabled while BrainzMash is active",
            )

        quarantined_alternate = bool(
            current.source_mode == "brainzmash"
            and current.source_quarantined
            and not brainzmash_runtime_enabled()
            and admission_context.source_mode == "brainzmash"
            and admission_context.source_url == current.api_url.rstrip("/")
            and admission_context.source_id == current.source_id
            and admission_context.generation == current.generation
        )

        def quarantined_probe_is_current() -> bool:
            latest = self._preferences_service.get_musicbrainz_connection()
            return bool(
                quarantined_alternate
                and self._preferences_service.get_musicbrainz_settings_revision()
                == admission_revision
                and self._preferences_service.musicbrainz_settings_match(current)
                and latest.source_mode == "brainzmash"
                and latest.source_quarantined
            )

        try:
            import httpx
            from infrastructure.validators import validate_service_url
            from core.exceptions import ValidationError as AppValidationError

            validate_service_url(settings.api_url, label="MusicBrainz API URL")
            app_settings = get_settings()
            client = get_http_client(app_settings)
            response = await mb_api_probe(
                settings.api_url,
                params={"query": "test", "limit": 1},
                client=client,
                source_context=MbSourceContext(
                    source_url=settings.api_url.rstrip("/"),
                    generation=admission_context.generation,
                    source_mode=settings.source_mode,
                    source_id=f"probe-{uuid.uuid4().hex}",
                ),
                admission_context=admission_context,
                admission_check=(
                    quarantined_probe_is_current if quarantined_alternate else None
                ),
                allow_quarantined_alternate=quarantined_alternate,
            )
            if response.status_code == 200:
                return MusicBrainzVerifyResult(
                    valid=True, message="Connected to MusicBrainz"
                )
            if response.status_code == 503:
                return MusicBrainzVerifyResult(
                    valid=True,
                    message="Connected, but rate-limited. Try lowering your rate limit.",
                )
            return MusicBrainzVerifyResult(
                valid=False,
                message=f"Unexpected response: HTTP {response.status_code}",
            )
        except AppValidationError as e:
            return MusicBrainzVerifyResult(valid=False, message=str(e))
        except httpx.ConnectError:
            return MusicBrainzVerifyResult(
                valid=False, message="Could not connect to the specified endpoint"
            )
        except Exception:  # noqa: BLE001
            logger.warning("Failed to verify MusicBrainz connection")
            return MusicBrainzVerifyResult(
                valid=False, message="Couldn't finish the connection test"
            )

    async def _apply_musicbrainz_settings(
        self, settings: MusicBrainzConnectionSettings
    ) -> None:
        from repositories.musicbrainz_base import (
            brainzmash_rate_limiter,
            brainzmash_runtime_enabled,
            brainzmash_circuit_breaker,
            get_mb_api_base,
            get_mb_source_generation,
            get_mb_source_mode,
            get_mb_source_id,
            set_mb_api_base,
            mb_rate_limiter,
            mb_rate_limiter_bypassed,
            mb_circuit_breaker,
            mb_deduplicator,
            mb_source_commit_lock,
            set_mb_rate_limiter_bypass,
        )
        from api.v1.schemas.settings import (
            _BRAINZMASH_RATE_LIMIT,
            _OFFICIAL_MB_CONCURRENT_SEARCHES,
            _OFFICIAL_MB_RATE_LIMIT,
            is_brainzmash_active_binding_valid,
            is_musicbrainz_rate_policy_public_host,
        )
        from infrastructure.http.brainzmash_transport import validate_brainzmash_url

        brainzmash_binding_valid = True
        if settings.source_mode == "brainzmash":
            try:
                validate_brainzmash_url(settings.api_url)
            except ValueError as exc:
                raise ConfigurationError(str(exc)) from exc
            brainzmash_binding_valid = is_brainzmash_active_binding_valid(settings)
        if settings.source_mode == "brainzmash":
            settings.rate_limit = _BRAINZMASH_RATE_LIMIT
            settings.concurrent_searches = 1
            brainzmash_rate_limiter.update_rate(_BRAINZMASH_RATE_LIMIT)
            brainzmash_rate_limiter.update_capacity(1)

        official_host = is_musicbrainz_rate_policy_public_host(settings.api_url)
        rate_policy_public_host = settings.source_mode == "brainzmash" or official_host
        if official_host:
            settings.rate_limit = min(settings.rate_limit, _OFFICIAL_MB_RATE_LIMIT)
            settings.concurrent_searches = min(
                settings.concurrent_searches, _OFFICIAL_MB_CONCURRENT_SEARCHES
            )
            if settings.rate_limit <= 0:
                settings.rate_limit = _OFFICIAL_MB_RATE_LIMIT

        limiter_rate = (
            _OFFICIAL_MB_RATE_LIMIT
            if settings.source_mode == "brainzmash"
            else settings.rate_limit
        )
        async with mb_source_commit_lock:
            current_bypass = mb_rate_limiter_bypassed()
            effective_rate = 0.0 if current_bypass else mb_rate_limiter.rate
            effective_capacity = (
                1 if rate_policy_public_host else settings.concurrent_searches
            )
            requested_bypass = settings.rate_limit == 0 and not rate_policy_public_host
            runtime_binding_changed = (
                settings.source_mode == "brainzmash"
                and brainzmash_runtime_enabled() != brainzmash_binding_valid
            )
            source_changed = (
                get_mb_api_base() != settings.api_url
                or get_mb_source_mode() != settings.source_mode
                or get_mb_source_id() != settings.source_id
                or get_mb_source_generation() != settings.generation
            )
            if (
                not source_changed
                and not runtime_binding_changed
                and effective_rate == limiter_rate
                and current_bypass == requested_bypass
                and mb_rate_limiter.capacity == effective_capacity
            ):
                logger.info(
                    "MusicBrainz connection settings unchanged; "
                    "skipping circuit breaker reset and cache clear"
                )
                return

            total = 0
            if source_changed:
                # Cache invalidation is the source-switch commit gate. A failed
                # clear leaves all live source/transport state untouched, so
                # retrying the persisted settings remains actionable. Control
                # changes intentionally retain provider results.
                for prefix in musicbrainz_prefixes():
                    total += await self._cache.clear_prefix(prefix)
                if self._disk_cache is not None:
                    await self._disk_cache.clear_musicbrainz()
                discovery_snapshot_store = getattr(
                    self, "_discovery_snapshot_store", None
                )
                if discovery_snapshot_store is not None:
                    await discovery_snapshot_store.delete_source_dependent_snapshots()
                # SearchService keeps process-local fresh and stale bucket
                # entries outside the shared cache prefixes. Clear them before
                # committing the new source so no old response is readable
                # after a successful switch; generation-aware keys also fence
                # calls that were already in flight.
                from services.search_service import SearchService

                SearchService.clear_cached_results()
            # Apply only after every source-switch clear succeeds, and keep the
            # live mutations synchronous while the source commit lock is held.
            set_mb_api_base(
                settings.api_url,
                source_mode=settings.source_mode,
                source_id=settings.source_id,
                generation=settings.generation,
                brainzmash_binding_valid=brainzmash_binding_valid,
            )
            if requested_bypass:
                set_mb_rate_limiter_bypass(True)
            else:
                set_mb_rate_limiter_bypass(False)
                mb_rate_limiter.update_rate(limiter_rate)
            mb_rate_limiter.update_capacity(effective_capacity)
            mb_circuit_breaker.reset()
            brainzmash_circuit_breaker.reset()
            mb_deduplicator.clear()

            if total:
                logger.info(
                    f"Cleared {total} MusicBrainz cache entries after settings change"
                )

    async def _restore_musicbrainz_after_failed_commit(
        self,
        expected: MusicBrainzConnectionSettings,
        replacement: MusicBrainzConnectionSettings,
        *,
        expected_revision: int,
    ) -> None:
        restored = self._preferences_service.restore_musicbrainz_connection_if_current(
            expected,
            replacement,
            expected_revision=expected_revision,
        )
        if not restored:
            return
        try:
            await self._apply_musicbrainz_settings(replacement)
        except BaseException:
            logger.exception("Failed to restore live MusicBrainz source state")

    async def on_musicbrainz_settings_changed(
        self, settings: MusicBrainzConnectionSettings
    ) -> None:
        """Apply already-persisted settings during startup or maintenance."""
        lock = getattr(self, "_musicbrainz_coordinator_lock", None)
        if lock is None:
            await self._apply_musicbrainz_settings(settings)
            return
        async with lock:
            await self._apply_musicbrainz_settings(settings)

    async def save_musicbrainz_update(
        self, update: MusicBrainzSettingsUpdate
    ) -> MusicBrainzConnectionSettings:
        """Persist and apply one normalized MusicBrainz source change as one CAS commit."""
        async with self._musicbrainz_coordinator_lock:
            previous = self._preferences_service.get_musicbrainz_connection()
            new_settings = self._preferences_service.save_musicbrainz_update(update)
            new_revision = self._preferences_service.get_musicbrainz_settings_revision()
            try:
                await self._apply_musicbrainz_settings(new_settings)
                if (
                    self._preferences_service.get_musicbrainz_settings_revision()
                    != new_revision
                    or not self._preferences_service.musicbrainz_settings_match(
                        new_settings
                    )
                ):
                    raise ConfigurationError(
                        "MusicBrainz settings changed before runtime apply completed"
                    )
            except BaseException:
                await self._restore_musicbrainz_after_failed_commit(
                    new_settings,
                    previous,
                    expected_revision=new_revision,
                )
                raise
            return self._preferences_service.get_musicbrainz_connection()

    async def stage_brainzmash(self) -> MusicBrainzConnectionSettings:
        """Persist and apply a BrainzMash proposal without probing upstream."""
        async with self._musicbrainz_coordinator_lock:
            previous = self._preferences_service.get_musicbrainz_connection()
            staged = self._preferences_service.stage_brainzmash()
            new_revision = self._preferences_service.get_musicbrainz_settings_revision()
            try:
                await self._apply_musicbrainz_settings(staged)
                if (
                    self._preferences_service.get_musicbrainz_settings_revision()
                    != new_revision
                    or not self._preferences_service.musicbrainz_settings_match(staged)
                ):
                    raise ConfigurationError(
                        "MusicBrainz settings changed before BrainzMash staging completed"
                    )
            except BaseException:
                await self._restore_musicbrainz_after_failed_commit(
                    staged,
                    previous,
                    expected_revision=new_revision,
                )
                raise
            return self._preferences_service.get_musicbrainz_connection()

    async def activate_brainzmash(
        self, binding: MusicBrainzBindingRequest
    ) -> MusicBrainzConnectionSettings:
        """Promote and apply a verified BrainzMash binding under one coordinator."""
        async with self._musicbrainz_coordinator_lock:
            previous, promoted = self._preferences_service.promote_brainzmash(binding)
            new_revision = self._preferences_service.get_musicbrainz_settings_revision()
            try:
                await self._apply_musicbrainz_settings(promoted)
                if (
                    self._preferences_service.get_musicbrainz_settings_revision()
                    != new_revision
                    or not self._preferences_service.musicbrainz_settings_match(
                        promoted
                    )
                ):
                    raise ConfigurationError(
                        "MusicBrainz settings changed before runtime activation completed"
                    )
            except BaseException:
                await self._restore_musicbrainz_after_failed_commit(
                    promoted,
                    previous,
                    expected_revision=new_revision,
                )
                raise
            return self._preferences_service.get_musicbrainz_connection()
