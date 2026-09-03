import logging
import os
import threading
import uuid
from pathlib import Path
from typing import Optional, TypeVar, Type
from typing import Any
from urllib.parse import urlsplit

import msgspec
from api.v1.schemas.settings import (
    UserPreferences,
    LibrarySyncSettings,
    LibraryScanScheduleSettings,
    DownloadClientConnectionSettings,
    JellyfinConnectionSettings,
    ListenBrainzConnectionSettings,
    OIDCConnectionSettings,
    YouTubeConnectionSettings,
    LocalFilesConnectionSettings,
    LastFmConnectionSettings,
    ScrobbleSettings,
    PrimaryMusicSourceSettings,
    LASTFM_SECRET_MASK,
    NavidromeConnectionSettings,
    NAVIDROME_PASSWORD_MASK,
    OIDC_SECRET_MASK,
    PlexConnectionSettings,
    PLEX_TOKEN_MASK,
    MusicBrainzConnectionSettings,
    MusicBrainzSettingsUpdate,
    MusicBrainzBindingRequest,
    BrainzMashPendingProposal,
    BrainzMashActiveBinding,
    BRAINZMASH_ENDPOINT,
    BRAINZMASH_DISCLOSURE_VERSION,
    _BRAINZMASH_RATE_LIMIT,
    SecuritySettings,
    LibrarySettings,
    ConnectAppsSettings,
    ACOUSTID_KEY_MASK,
    DOWNLOAD_CLIENT_API_KEY_MASK,
    INDEXER_API_KEY_MASK,
    SABNZBD_API_KEY_MASK,
    LIDARR_IMPORT_API_KEY_MASK,
    DownloadPolicySettings,
    LidarrImportConnectionSettings,
    NewznabIndexerSettings,
    SabnzbdConnectionSettings,
    QualityRecipeEntry,
    SpotifySettings,
    validate_quality_recipe,
    SPOTIFY_SECRET_MASK,
    EventsSettings,
    FreeMusicSettings,
    GetItSettings,
    PluginConfig,
    TICKETMASTER_KEY_MASK,
    SKIDDLE_KEY_MASK,
    DEFAULT_NAMING_TEMPLATE,
    WrappedSettings,
    WRAPPED_API_KEY_MASK,
    WantedWatcherSettings,
)
from api.v1.schemas.advanced_settings import AdvancedSettings
from api.v1.schemas.library_policies import LibraryRootSettings, TypedLibrarySettings
from api.v1.schemas.library_management import (
    LibraryManagementSettings,
    LibraryManagementSettingsResponse,
    build_initial_library_management_settings,
    migrate_library_management_presets,
    normalize_library_management_settings,
    settings_revision as library_management_settings_revision,
    to_library_management_response,
)
from core.config import Settings
from core.exceptions import (
    ConfigurationError,
    ScriptValidationError,
    StaleRevisionError,
    ValidationError,
)
from infrastructure.crypto import decrypt, encrypt
from infrastructure.file_utils import atomic_write_json, read_json
from infrastructure.serialization import to_jsonable
from models.release_type_policy import normalize_release_type_filters

logger = logging.getLogger(__name__)

T = TypeVar("T", bound=msgspec.Struct)
SPOTIFY_CALLBACK_PATH = "/api/v1/me/connections/spotify/auth/callback"

_RELEASE_TYPE_POLICY_REVISION_KEY = "release_type_policy_revision"


class PreferencesService:
    def __init__(self, settings: Settings):
        self._settings = settings
        self._config_path = settings.config_file_path
        self._config_cache: Optional[dict] = None
        self._cache_lock = threading.RLock()
        # Section saves are synchronous, so use a dedicated non-reentrant
        # cross-thread transaction lock rather than treating the cache RLock
        # as an asyncio boundary.
        self._section_save_lock = threading.Lock()
        # Fresh configuration gets the normal acquisition defaults above. MusicBrainz
        # normalization below separately makes the built-in BrainzMash source effective
        # for absent and legacy non-custom source settings.
        self._seed_initial_acquisition_defaults()
        self._normalize_get_it_settings()
        self._migrate_musicbrainz_settings()
        self._ensure_instance_id()

    def _ensure_instance_id(self) -> None:
        """Generate a stable instance ID on first run."""
        config = self._load_config()
        if config.get("instance_id"):
            return
        import uuid

        instance_id = str(uuid.uuid4())
        config = self._load_config().copy()
        config["instance_id"] = instance_id
        self._save_config(config)
        logger.info("Generated new instance ID: %s", instance_id)

    def get_instance_id(self) -> str:
        config = self._load_config()
        return config.get("instance_id", "unknown")

    def _load_config(self) -> dict:
        with self._cache_lock:
            if self._config_cache is not None:
                return self._config_cache

            if not self._config_path.exists():
                self._config_cache = {}
                return self._config_cache

            try:
                loaded = read_json(self._config_path, default={})
                self._config_cache = loaded if isinstance(loaded, dict) else {}
            except Exception as e:  # noqa: BLE001
                logger.error(f"Failed to load config: {e}")
                self._config_cache = {}

            return self._config_cache

    def _save_config(self, config: dict) -> None:
        with self._cache_lock:
            self._config_path.parent.mkdir(parents=True, exist_ok=True)
            atomic_write_json(self._config_path, config)
            self._config_cache = config

    def _get_section(
        self, key: str, model: Type[T], default_factory: Optional[callable] = None
    ) -> T:
        config = self._load_config()
        data = config.get(key, {})
        try:
            if not (isinstance(model, type) and issubclass(model, msgspec.Struct)):
                raise TypeError(
                    f"Preferences section model must be msgspec.Struct, got {model!r}"
                )

            if data:
                return msgspec.convert(data, type=model)
            return default_factory() if default_factory else model()
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to parse {key}: {e}")
            return default_factory() if default_factory else model()

    def _save_section(self, key: str, value: Any) -> None:
        with self._section_save_lock:
            config = self._load_config().copy()
            config[key] = to_jsonable(value)
            self._save_config(config)

    def _read_secret(self, path: tuple[str, ...], stored_value: str) -> str:
        if not stored_value:
            return stored_value
        plaintext, was_legacy = decrypt(stored_value)
        if was_legacy:
            config = self._load_config().copy()
            node = config
            for key in path[:-1]:
                node = node.setdefault(key, {})
            node[path[-1]] = encrypt(plaintext)
            self._save_config(config)
        return plaintext

    @staticmethod
    def _release_type_filters(
        preferences: UserPreferences,
    ) -> tuple[frozenset[str], frozenset[str]]:
        return normalize_release_type_filters(
            preferences.primary_types, preferences.secondary_types
        )

    @staticmethod
    def _release_type_policy_revision_from_config(config: dict) -> int:
        internal = config.get("_internal", {})
        if not isinstance(internal, dict):
            return 0
        try:
            return max(0, int(internal.get(_RELEASE_TYPE_POLICY_REVISION_KEY, 0)))
        except (TypeError, ValueError):
            return 0

    def get_preferences(self) -> UserPreferences:
        return self._get_section("user_preferences", UserPreferences)

    def get_preferences_with_revision(self) -> tuple[UserPreferences, int]:
        """Read the release-type policy and its persisted revision together."""
        with self._section_save_lock:
            config = self._load_config()
            return (
                self._get_section("user_preferences", UserPreferences),
                self._release_type_policy_revision_from_config(config),
            )

    def save_preferences(self, preferences: UserPreferences) -> None:
        try:
            with self._section_save_lock:
                config = self._load_config().copy()
                try:
                    previous = msgspec.convert(
                        config.get("user_preferences", {}), type=UserPreferences
                    )
                except (msgspec.ValidationError, TypeError, ValueError):
                    previous = UserPreferences()

                if self._release_type_filters(previous) != self._release_type_filters(
                    preferences
                ):
                    internal = config.get("_internal", {})
                    internal = internal.copy() if isinstance(internal, dict) else {}
                    revision = self._release_type_policy_revision_from_config(config)
                    internal[_RELEASE_TYPE_POLICY_REVISION_KEY] = revision + 1
                    config["_internal"] = internal
                config["user_preferences"] = to_jsonable(preferences)
                self._save_config(config)
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to save preferences: {e}")
            raise ConfigurationError(f"Failed to save preferences: {e}")

    def get_library_sync_settings(self) -> LibrarySyncSettings:
        return self._get_section("library_sync_settings", LibrarySyncSettings)

    def save_library_sync_settings(
        self, library_sync_settings: LibrarySyncSettings
    ) -> None:
        try:
            self._save_section("library_sync_settings", library_sync_settings)
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to save library sync settings: {e}")
            raise ConfigurationError(f"Failed to save library sync settings: {e}")

    def get_library_scan_schedule(self) -> LibraryScanScheduleSettings:
        config = self._load_config()
        if "library_scan_schedule" not in config and "library_sync_settings" in config:
            # one-time migration: carry the old interval to the native schedule
            freq = config["library_sync_settings"].get("sync_frequency", "24hr")
            migrated = LibraryScanScheduleSettings(scan_frequency=freq)
            self._save_section("library_scan_schedule", migrated)
            return migrated
        return self._get_section("library_scan_schedule", LibraryScanScheduleSettings)

    def save_library_scan_schedule(self, schedule: LibraryScanScheduleSettings) -> None:
        try:
            self._save_section("library_scan_schedule", schedule)
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to save library scan schedule: {e}")
            raise ConfigurationError(f"Failed to save library scan schedule: {e}")

    def get_advanced_settings(self) -> AdvancedSettings:
        return self._get_section("advanced_settings", AdvancedSettings)

    def save_advanced_settings(self, advanced_settings: AdvancedSettings) -> None:
        try:
            self._save_section("advanced_settings", advanced_settings)
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to save advanced settings: {e}")
            raise ConfigurationError(f"Failed to save advanced settings: {e}")

    def get_download_client_settings(self) -> DownloadClientConnectionSettings:
        """Download client settings with the slskd ``api_key`` MASKED (safe for API responses)."""
        settings = self._get_section(
            "download_client", DownloadClientConnectionSettings
        )
        if settings.api_key:
            settings.api_key = DOWNLOAD_CLIENT_API_KEY_MASK
        return settings

    def get_download_client_settings_raw(self) -> DownloadClientConnectionSettings:
        """Download client settings with the slskd ``api_key`` DECRYPTED (for the client/scorer)."""
        config = self._load_config()
        data = config.get("download_client", {})
        settings = self._get_section(
            "download_client", DownloadClientConnectionSettings
        )
        settings.api_key = self._read_secret(
            ("download_client", "api_key"), data.get("api_key", "")
        )
        # Strip paste whitespace post-decrypt so a key saved before the fix
        # still authenticates (SABnzbd raw-getter precedent).
        if settings.api_key:
            settings.api_key = settings.api_key.strip()
        return settings

    def save_download_client_settings(
        self, settings: DownloadClientConnectionSettings
    ) -> None:
        try:
            config = self._load_config().copy()
            current = config.get("download_client", {})
            api_key = settings.api_key.strip() if settings.api_key else ""
            if api_key == DOWNLOAD_CLIENT_API_KEY_MASK:
                api_key = current.get(
                    "api_key", ""
                )  # preserve existing on masked sentinel
            elif api_key:
                api_key = encrypt(api_key)
            config["download_client"] = {
                "enabled": settings.enabled,
                "client_type": settings.client_type,
                "url": settings.url,
                "api_key": api_key,
                "verify_downloads": settings.verify_downloads,
                "min_bitrate_kbps": settings.min_bitrate_kbps,
                "quality_min": settings.quality_min,
                "quality_max": settings.quality_max,
                "flac_mp3_only": settings.flac_mp3_only,
                "downloads_subpath": settings.downloads_subpath,
                "slskd_incomplete_mount": settings.slskd_incomplete_mount,
                "preflight_score_auto_accept": settings.preflight_score_auto_accept,
                "preflight_score_manual_min": settings.preflight_score_manual_min,
                "download_stall_timeout_minutes": settings.download_stall_timeout_minutes,
                "download_queued_timeout_minutes": settings.download_queued_timeout_minutes,
                "preferred_quality_wait_minutes": settings.preferred_quality_wait_minutes,
                "max_failover_attempts": settings.max_failover_attempts,
                "max_concurrent_downloads": settings.max_concurrent_downloads,
            }
            self._save_config(config)
            logger.info("Saved download client settings to %s", self._config_path)
        except Exception as e:  # noqa: BLE001
            logger.error("Failed to save download client settings: %s", e)
            raise ConfigurationError(f"Failed to save download client settings: {e}")

    def get_slskd_incomplete_mount(self) -> Path | None:
        """Resolved slskd incomplete-downloads dir, or None when unset/unusable.

        Fail-closed: empty, unresolvable, non-dir, or unreadable all yield None,
        and the repository then skips the incomplete fallback entirely
        (byte-identical behaviour to before the knob existed).
        """
        raw = self.get_download_client_settings_raw().slskd_incomplete_mount
        if not raw:
            return None
        try:
            resolved = Path(raw).resolve()
        except (OSError, RuntimeError):
            return None
        if not resolved.is_dir() or not os.access(resolved, os.R_OK):
            return None
        return resolved

    def _decode_download_policy(self, data: object) -> DownloadPolicySettings:
        """Decode policy while preserving invalid recipe status.

        Legacy fields remain usable when a hand-edited/stale v2 recipe cannot
        decode. The raw invalid blob is never rewritten by this read path.
        """
        if not isinstance(data, dict):
            return DownloadPolicySettings(
                quality_recipe_status="invalid",
                quality_recipe_error="download_policy must be an object",
            )
        try:
            policy = msgspec.convert(data, type=DownloadPolicySettings, strict=True)
            if policy.quality_recipe:
                validate_quality_recipe(policy.quality_recipe)
                if not policy.flac_mp3_only:
                    policy.quality_recipe_status = "non_convertible"
                    policy.quality_recipe_error = (
                        "v2 recipes require FLAC/MP3-only mode"
                    )
                else:
                    policy.quality_recipe_status = "v2"
                    policy.quality_recipe_error = None
            else:
                policy.quality_recipe_status = (
                    "v1" if policy.flac_mp3_only else "non_convertible"
                )
                policy.quality_recipe_error = (
                    None
                    if policy.flac_mp3_only
                    else "legacy policy allows codecs outside FLAC/MP3"
                )
            return policy
        except (msgspec.ValidationError, TypeError, ValueError) as exc:
            # Strip only the malformed v2 field; all legacy policy controls
            # still decode and the caller can show an actionable status.
            legacy = dict(data)
            legacy.pop("quality_recipe", None)
            try:
                policy = msgspec.convert(
                    legacy, type=DownloadPolicySettings, strict=True
                )
            except (msgspec.ValidationError, TypeError, ValueError):
                policy = DownloadPolicySettings()
            policy.quality_recipe = []
            policy.quality_recipe_status = "invalid"
            policy.quality_recipe_error = str(exc)
            return policy

    def get_download_policy(self) -> DownloadPolicySettings:
        """Read the shared policy without healing away invalid v2 data."""
        config = self._load_config()
        if "download_policy" in config:
            return self._decode_download_policy(config.get("download_policy"))
        dc = config.get("download_client", {})
        if dc:
            policy = DownloadPolicySettings(
                quality_min=dc.get("quality_min", "mp3_320"),
                quality_max=dc.get("quality_max", "lossless"),
                flac_mp3_only=dc.get("flac_mp3_only", True),
                verify_downloads=dc.get("verify_downloads", True),
                preflight_score_auto_accept=dc.get("preflight_score_auto_accept", 0.70),
                preflight_score_manual_min=dc.get("preflight_score_manual_min", 0.50),
                download_stall_timeout_minutes=dc.get(
                    "download_stall_timeout_minutes", 30
                ),
                download_queued_timeout_minutes=dc.get(
                    "download_queued_timeout_minutes", 120
                ),
                preferred_quality_wait_minutes=dc.get(
                    "preferred_quality_wait_minutes", 15
                ),
                max_failover_attempts=dc.get("max_failover_attempts", 3),
                max_concurrent_downloads=dc.get("max_concurrent_downloads", 3),
                auto_retry_enabled=dc.get("auto_retry_enabled", True),
                auto_retry_max_attempts=dc.get("auto_retry_max_attempts", 6),
                auto_retry_base_interval_minutes=dc.get(
                    "auto_retry_base_interval_minutes", 15
                ),
            )
            policy.quality_recipe_status = (
                "v1" if policy.flac_mp3_only else "non_convertible"
            )
            policy.quality_recipe_error = (
                None
                if policy.flac_mp3_only
                else "legacy policy allows codecs outside FLAC/MP3"
            )
            return policy
        return DownloadPolicySettings()

    def save_download_policy(self, policy: DownloadPolicySettings) -> None:
        try:
            config = self._load_config().copy()
            section = to_jsonable(policy)
            section.pop("quality_recipe_status", None)
            section.pop("quality_recipe_error", None)
            if policy.quality_recipe:
                if not policy.flac_mp3_only:
                    raise ConfigurationError(
                        "A v2 recipe cannot be saved while non-FLAC/MP3 formats are enabled"
                    )
                from services.native.acquisition.quality import (
                    legacy_range_from_recipe,
                    legacy_recipe_order,
                )

                canonical = validate_quality_recipe(policy.quality_recipe)
                quality_min, quality_max = legacy_range_from_recipe(canonical)
                section["quality_recipe"] = to_jsonable(canonical)
                section["quality_min"] = quality_min
                section["quality_max"] = quality_max
                section["quality_preference_order"] = legacy_recipe_order(canonical)
                section["flac_mp3_only"] = True
            else:
                section["quality_min"] = policy.quality_min
                section["quality_max"] = policy.quality_max
                section["flac_mp3_only"] = policy.flac_mp3_only

            # Keep the legacy upgrade cutoff inside the canonical range projected
            # above, including after a recipe narrows that range.
            from services.native.quality_tiers import TIER_KEYS, tier_rank

            cutoff_rank = tier_rank(policy.quality_cutoff)
            minimum_rank = tier_rank(section["quality_min"])
            maximum_rank = tier_rank(section["quality_max"])
            cutoff_rank = min(max(cutoff_rank, minimum_rank), maximum_rank)
            section["quality_cutoff"] = TIER_KEYS[len(TIER_KEYS) - 1 - cutoff_rank]

            # Legacy rollback mirrors (Acquisition plan): after every new-policy
            # save, keep the closest legacy-representable values populated.
            section["preferred_quality_wait_minutes"] = (
                policy.preferred_quality_wait_minutes
            )
            config["download_policy"] = section
            free_music = config.get("free_music")
            if not isinstance(free_music, dict):
                free_music = {}
            free_music["preferred_format"] = (
                policy.quality_recipe[0].format
                if policy.quality_recipe
                else (
                    "flac"
                    if policy.quality_preference_order
                    and policy.quality_preference_order[0] == "lossless"
                    else "mp3"
                )
            )
            config["free_music"] = free_music
            self._save_config(config)
            logger.info("Saved download policy to %s", self._config_path)
        except ConfigurationError:
            raise
        except Exception as e:  # noqa: BLE001
            logger.error("Failed to save download policy: %s", e)
            raise ConfigurationError(f"Failed to save download policy: {e}")

    def _seed_initial_acquisition_defaults(self) -> None:
        """First-boot seeding of ``download_policy`` for NEW installs only
        (owner decisions 2026-08-27): Balanced preset - lossless→320→256→192,
        lossy target 320, CD-quality lossless preferred and capped at
        16-bit/48 kHz, unknown evidence parked for review, source-first."""
        if self._config_path.exists():
            return
        try:
            self._save_config(
                {
                    "download_policy": {
                        "quality_min": "mp3_192",
                        "quality_max": "lossless",
                        "flac_mp3_only": True,
                        "quality_preference_order": [
                            "lossless",
                            "mp3_320",
                            "mp3_256",
                            "mp3_192",
                        ],
                        "preferred_lossy_bitrate_kbps": 320,
                        "lossless_preference": "cd",
                        "lossless_max_bit_depth": 16,
                        "lossless_max_sample_rate_hz": 48000,
                        "unknown_quality_behavior": "review",
                        "source_selection_mode": "source_first",
                    }
                }
            )
            logger.info("Seeded new-install acquisition defaults (Balanced preset)")
        except Exception as exc:  # noqa: BLE001 - seeding must not block startup
            logger.error("Failed to seed acquisition defaults: %s", exc)

    # --- Wanted watcher (Wanted plan §5.4) - mask-free, no secrets -------------------

    def get_wanted_settings(self) -> WantedWatcherSettings:
        return self._get_section("wanted", WantedWatcherSettings)

    def save_wanted_settings(self, settings: WantedWatcherSettings) -> None:
        try:
            self._save_section("wanted", settings)
        except Exception as e:  # noqa: BLE001
            logger.error("Failed to save wanted watcher settings: %s", e)
            raise ConfigurationError(f"Failed to save wanted watcher settings: {e}")

    # --- Source priority (D3/D19) - the order sources are tried in -------------------

    def get_source_priority(self) -> list[str]:
        """The order acquisition sources are tried (D3). Defaults to Soulseek-first;
        unknown/missing sources are appended so the list always covers both."""
        raw = self._load_config().get("source_priority")
        order = (
            [s for s in raw if s in ("soulseek", "usenet")]
            if isinstance(raw, list)
            else []
        )
        for source in ("soulseek", "usenet"):
            if source not in order:
                order.append(source)
        return order

    def save_source_priority(self, order: list[str]) -> None:
        clean = [s for s in order if s in ("soulseek", "usenet")]
        for source in ("soulseek", "usenet"):
            if source not in clean:
                clean.append(source)
        config = self._load_config().copy()
        config["source_priority"] = clean
        self._save_config(config)

    # --- SABnzbd download client (D5) - in the download_clients map -----------------

    def get_sabnzbd_connection(self) -> SabnzbdConnectionSettings:
        """SABnzbd connection with the ``api_key`` MASKED (safe for API responses)."""
        data = self._load_config().get("download_clients", {}).get("sabnzbd", {})
        settings = (
            msgspec.convert(data, type=SabnzbdConnectionSettings)
            if data
            else SabnzbdConnectionSettings()
        )
        if settings.api_key:
            settings.api_key = SABNZBD_API_KEY_MASK
        return settings

    def get_sabnzbd_connection_raw(self) -> SabnzbdConnectionSettings:
        """SABnzbd connection with the ``api_key`` DECRYPTED (for the client)."""
        data = self._load_config().get("download_clients", {}).get("sabnzbd", {})
        settings = (
            msgspec.convert(data, type=SabnzbdConnectionSettings)
            if data
            else SabnzbdConnectionSettings()
        )
        stored = data.get("api_key", "")
        # Strip stray paste whitespace so a key saved before this fix still authenticates.
        settings.api_key = decrypt(stored)[0].strip() if stored else ""
        return settings

    def save_sabnzbd_connection(self, settings: SabnzbdConnectionSettings) -> None:
        try:
            config = self._load_config().copy()
            clients = dict(config.get("download_clients", {}))
            current = clients.get("sabnzbd", {})
            api_key = settings.api_key.strip()
            if api_key == SABNZBD_API_KEY_MASK:
                api_key = current.get("api_key", "")  # preserve on masked sentinel
            elif api_key:
                api_key = encrypt(api_key)
            clients["sabnzbd"] = {
                "enabled": settings.enabled,
                "client_type": "sabnzbd",
                "url": settings.url,
                "api_key": api_key,
                "category": settings.category,
                "priority": settings.priority,
                "post_processing": settings.post_processing,
                "downloads_mount": settings.downloads_mount,
            }
            config["download_clients"] = clients
            self._save_config(config)
            logger.info("Saved SABnzbd connection settings")
        except Exception as e:  # noqa: BLE001
            logger.error("Failed to save SABnzbd settings: %s", e)
            raise ConfigurationError(f"Failed to save SABnzbd settings: {e}")

    # --- Lidarr import connection (LidarrImport D5) - read-only migration aid ------

    def get_lidarr_import_connection(self) -> LidarrImportConnectionSettings:
        """Lidarr import connection with the ``api_key`` MASKED (safe for API responses)."""
        data = self._load_config().get("lidarr_import", {})
        settings = (
            msgspec.convert(data, type=LidarrImportConnectionSettings)
            if data
            else LidarrImportConnectionSettings()
        )
        if settings.api_key:
            settings.api_key = LIDARR_IMPORT_API_KEY_MASK
        return settings

    def get_lidarr_import_connection_raw(self) -> LidarrImportConnectionSettings:
        """Lidarr import connection with the ``api_key`` DECRYPTED (for the client)."""
        data = self._load_config().get("lidarr_import", {})
        settings = (
            msgspec.convert(data, type=LidarrImportConnectionSettings)
            if data
            else LidarrImportConnectionSettings()
        )
        stored = data.get("api_key", "")
        settings.api_key = decrypt(stored)[0].strip() if stored else ""
        return settings

    def save_lidarr_import_connection(
        self, settings: LidarrImportConnectionSettings
    ) -> None:
        try:
            config = self._load_config().copy()
            current = config.get("lidarr_import", {})
            api_key = settings.api_key.strip()
            if api_key == LIDARR_IMPORT_API_KEY_MASK:
                api_key = current.get("api_key", "")  # preserve on masked sentinel
            elif api_key:
                api_key = encrypt(api_key)
            config["lidarr_import"] = {"url": settings.url, "api_key": api_key}
            self._save_config(config)
            logger.info("Saved Lidarr import connection settings")
        except Exception as e:  # noqa: BLE001
            logger.error("Failed to save Lidarr import settings: %s", e)
            raise ConfigurationError(f"Failed to save Lidarr import settings: {e}")

    def is_lidarr_import_configured(self) -> bool:
        """True iff a Lidarr import URL + API key are both stored (the non-admin gate)."""
        raw = self.get_lidarr_import_connection_raw()
        return bool(raw.url and raw.api_key)

    # --- Newznab indexers (D6) - a list, each with its own encrypted api_key ------

    def get_indexers(self) -> list["NewznabIndexerSettings"]:
        """All configured indexers, each ``api_key`` MASKED (safe for API responses),
        ordered by priority."""
        out = []
        for item in self._load_config().get("indexers", []):
            settings = msgspec.convert(item, type=NewznabIndexerSettings)
            if settings.api_key:
                settings.api_key = INDEXER_API_KEY_MASK
            out.append(settings)
        return sorted(out, key=lambda s: s.priority)

    def get_indexers_raw(self) -> list["NewznabIndexerSettings"]:
        """All configured indexers with ``api_key`` DECRYPTED (for building clients),
        ordered by priority."""
        out = []
        for item in self._load_config().get("indexers", []):
            settings = msgspec.convert(item, type=NewznabIndexerSettings)
            stored = item.get("api_key", "")
            # Strip on read too, so a key saved with stray whitespace before this fix
            # still authenticates without the user having to re-enter it.
            settings.api_key = decrypt(stored)[0].strip() if stored else ""
            out.append(settings)
        return sorted(out, key=lambda s: s.priority)

    def save_indexer(self, settings: "NewznabIndexerSettings") -> str:
        """Upsert one indexer by ``id`` (a new id is minted when blank). The
        ``api_key`` is encrypted, or preserved when the masked sentinel comes back.
        Returns the indexer id."""
        import uuid

        try:
            config = self._load_config().copy()
            indexers = list(config.get("indexers", []))
            existing = next((i for i in indexers if i.get("id") == settings.id), None)

            # Trim pasted whitespace - a leading space/tab on the key reaches the indexer
            # verbatim and earns an HTTP 403 (the apikey no longer matches).
            api_key = settings.api_key.strip()
            if api_key == INDEXER_API_KEY_MASK:
                api_key = existing.get("api_key", "") if existing else ""
            elif api_key:
                api_key = encrypt(api_key)

            indexer_id = settings.id or uuid.uuid4().hex
            row = {
                "id": indexer_id,
                "type": settings.type,
                "name": settings.name,
                "url": settings.url,
                "api_key": api_key,
                "categories": settings.categories,
                "enabled": settings.enabled,
                "priority": settings.priority,
            }
            if existing is not None:
                indexers = [row if i.get("id") == indexer_id else i for i in indexers]
            else:
                indexers.append(row)
            config["indexers"] = indexers
            self._save_config(config)
            logger.info("Saved indexer %s", indexer_id)
            return indexer_id
        except Exception as e:  # noqa: BLE001
            logger.error("Failed to save indexer: %s", e)
            raise ConfigurationError(f"Failed to save indexer: {e}")

    def delete_indexer(self, indexer_id: str) -> None:
        config = self._load_config().copy()
        config["indexers"] = [
            i for i in config.get("indexers", []) if i.get("id") != indexer_id
        ]
        self._save_config(config)

    def reorder_indexers(self, ordered_ids: list[str]) -> None:
        """Persist a new priority order (1-based) from the dragged card order; ids
        not present keep their relative order after the listed ones."""
        config = self._load_config().copy()
        indexers = list(config.get("indexers", []))
        rank = {iid: pos for pos, iid in enumerate(ordered_ids)}
        for item in indexers:
            if item.get("id") in rank:
                item["priority"] = rank[item["id"]] + 1
        config["indexers"] = indexers
        self._save_config(config)

    # --- download-source readiness (single source of truth) ----------------------
    # "Can the user acquire music?" is answered here, NOT per-feature, so the slskd and
    # Usenet checks can't drift apart (Home, Discover, and the orchestrator all read these).

    def is_soulseek_ready(self) -> bool:
        """slskd (Soulseek) is enabled and has a URL."""
        dc = self.get_download_client_settings()
        return dc.enabled and bool(dc.url)

    def is_usenet_ready(self) -> bool:
        """SABnzbd (Usenet) is enabled with a URL AND at least one enabled indexer to
        search - SABnzbd with no indexer can't find anything to download."""
        sab = self.get_sabnzbd_connection()
        return (
            sab.enabled
            and bool(sab.url)
            and any(i.enabled for i in self.get_indexers())
        )

    def is_builtin_download_ready(self) -> bool:
        """A user-configured download client (Soulseek OR Usenet) is set up.
        Chooses the dispatcher; dies with those clients in 2.0."""
        return self.is_soulseek_ready() or self.is_usenet_ready()

    def is_download_source_ready(self) -> bool:
        """At least one acquisition source is set up: Soulseek, Usenet, or Free
        Music (D24). The single source of truth for "can the user acquire" -
        after 2.0 this reduces to Free Music alone."""
        return (
            self.is_builtin_download_ready() or self.get_free_music_settings().enabled
        )

    def get_free_music_settings(self) -> FreeMusicSettings:
        data = self._load_config().get("free_music", {})
        fmt = str(data.get("preferred_format") or "flac").lower()
        return FreeMusicSettings(
            enabled=bool(data.get("enabled", True)),
            preferred_format="mp3" if fmt == "mp3" else "flac",
        )

    def save_free_music_settings(self, settings: FreeMusicSettings) -> None:
        try:
            config = self._load_config().copy()
            config["free_music"] = {
                "enabled": settings.enabled,
                "preferred_format": settings.preferred_format,
            }
            self._save_config(config)
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to save Free Music settings: {e}")
            raise ConfigurationError("Failed to save Free Music settings")

    def get_jellyfin_connection(self) -> JellyfinConnectionSettings:
        config = self._load_config()
        jellyfin_data = config.get("jellyfin_settings", {})
        api_key = self._read_secret(
            ("jellyfin_settings", "api_key"), jellyfin_data.get("api_key", "")
        )
        return JellyfinConnectionSettings(
            jellyfin_url=jellyfin_data.get(
                "jellyfin_url", config.get("jellyfin_url", self._settings.jellyfin_url)
            ),
            api_key=api_key,
            user_id=jellyfin_data.get("user_id", ""),
            enabled=jellyfin_data.get("enabled", False),
            login_enabled=jellyfin_data.get("login_enabled", False),
        )

    def save_jellyfin_connection(self, settings: JellyfinConnectionSettings) -> None:
        try:
            config = self._load_config().copy()
            config["jellyfin_url"] = settings.jellyfin_url
            config["jellyfin_settings"] = {
                "jellyfin_url": settings.jellyfin_url,
                "api_key": encrypt(settings.api_key),
                "user_id": settings.user_id,
                "enabled": settings.enabled,
                "login_enabled": settings.login_enabled,
            }
            self._save_config(config)

            self._settings.jellyfin_url = settings.jellyfin_url
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to save Jellyfin connection settings: {e}")
            raise ConfigurationError(
                f"Failed to save Jellyfin connection settings: {e}"
            )

    def get_navidrome_connection(self) -> NavidromeConnectionSettings:
        config = self._load_config()
        nd_data = config.get("navidrome_settings", {})
        password = nd_data.get("password", "")
        return NavidromeConnectionSettings(
            navidrome_url=nd_data.get("navidrome_url", ""),
            username=nd_data.get("username", ""),
            password=NAVIDROME_PASSWORD_MASK if password else "",
            enabled=nd_data.get("enabled", False),
        )

    def get_navidrome_connection_raw(self) -> NavidromeConnectionSettings:
        config = self._load_config()
        nd_data = config.get("navidrome_settings", {})
        password = self._read_secret(
            ("navidrome_settings", "password"), nd_data.get("password", "")
        )
        return NavidromeConnectionSettings(
            navidrome_url=nd_data.get("navidrome_url", ""),
            username=nd_data.get("username", ""),
            password=password,
            enabled=nd_data.get("enabled", False),
        )

    def save_navidrome_connection(self, settings: NavidromeConnectionSettings) -> None:
        try:
            config = self._load_config().copy()
            current_data = config.get("navidrome_settings", {})

            password = settings.password
            if password == NAVIDROME_PASSWORD_MASK:
                password = current_data.get("password", "")
            else:
                password = encrypt(password)

            config["navidrome_settings"] = {
                "navidrome_url": settings.navidrome_url,
                "username": settings.username,
                "password": password,
                "enabled": settings.enabled,
            }
            self._save_config(config)
        except Exception as e:  # noqa: BLE001
            logger.error("Failed to save Navidrome connection settings: %s", e)
            raise ConfigurationError(
                f"Failed to save Navidrome connection settings: {e}"
            )

    def get_plex_connection(self) -> PlexConnectionSettings:
        config = self._load_config()
        plex_data = config.get("plex_settings", {})
        settings = PlexConnectionSettings(
            plex_url=plex_data.get("plex_url", ""),
            plex_token=plex_data.get("plex_token", ""),
            enabled=plex_data.get("enabled", False),
            login_enabled=plex_data.get("login_enabled", False),
            music_library_ids=plex_data.get("music_library_ids", []),
            scrobble_to_plex=plex_data.get("scrobble_to_plex", True),
        )
        if settings.plex_token:
            settings.plex_token = PLEX_TOKEN_MASK
        return settings

    def get_plex_connection_raw(self) -> PlexConnectionSettings:
        config = self._load_config()
        plex_data = config.get("plex_settings", {})
        token = self._read_secret(
            ("plex_settings", "plex_token"), plex_data.get("plex_token", "")
        )
        return PlexConnectionSettings(
            plex_url=plex_data.get("plex_url", ""),
            plex_token=token,
            enabled=plex_data.get("enabled", False),
            login_enabled=plex_data.get("login_enabled", False),
            music_library_ids=plex_data.get("music_library_ids", []),
            scrobble_to_plex=plex_data.get("scrobble_to_plex", True),
        )

    def save_plex_connection(self, settings: PlexConnectionSettings) -> None:
        try:
            config = self._load_config().copy()
            current_data = config.get("plex_settings", {})

            token = settings.plex_token
            if token == PLEX_TOKEN_MASK:
                token = current_data.get("plex_token", "")
            else:
                token = encrypt(token)

            config["plex_settings"] = {
                "plex_url": settings.plex_url,
                "plex_token": token,
                "enabled": settings.enabled,
                "login_enabled": settings.login_enabled,
                "music_library_ids": settings.music_library_ids,
                "scrobble_to_plex": settings.scrobble_to_plex,
            }
            self._save_config(config)
            logger.info("Saved Plex connection settings to %s", self._config_path)
        except Exception as e:  # noqa: BLE001
            logger.error("Failed to save Plex connection settings: %s", e)
            raise ConfigurationError(f"Failed to save Plex connection settings: {e}")

    def get_listenbrainz_connection(self) -> ListenBrainzConnectionSettings:
        config = self._load_config()
        lb_data = config.get("listenbrainz_settings", {})
        user_token = self._read_secret(
            ("listenbrainz_settings", "user_token"), lb_data.get("user_token", "")
        )
        return ListenBrainzConnectionSettings(
            username=lb_data.get("username", ""),
            user_token=user_token,
            enabled=lb_data.get("enabled", False),
        )

    def save_listenbrainz_connection(
        self, settings: ListenBrainzConnectionSettings
    ) -> None:
        try:
            config = self._load_config().copy()
            config["listenbrainz_settings"] = {
                "username": settings.username,
                "user_token": encrypt(settings.user_token),
                "enabled": settings.enabled,
            }
            self._save_config(config)
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to save ListenBrainz connection settings: {e}")
            raise ConfigurationError(
                f"Failed to save ListenBrainz connection settings: {e}"
            )

    def get_youtube_connection(self) -> YouTubeConnectionSettings:
        config = self._load_config()
        yt_data = config.get("youtube_settings", {})
        api_key = self._read_secret(
            ("youtube_settings", "api_key"), str(yt_data.get("api_key") or "")
        )
        enabled = yt_data.get("enabled", False)
        # Auto-migrate: existing setups with enabled+api_key get api_enabled=True
        if "api_enabled" not in yt_data and enabled and api_key.strip():
            api_enabled = True
        else:
            api_enabled = yt_data.get("api_enabled", False)
        return YouTubeConnectionSettings(
            api_key=api_key,
            enabled=enabled,
            api_enabled=api_enabled,
            daily_quota_limit=yt_data.get("daily_quota_limit", 80),
        )

    def save_youtube_connection(self, settings: YouTubeConnectionSettings) -> None:
        try:
            config = self._load_config().copy()
            config["youtube_settings"] = {
                "api_key": encrypt(settings.api_key.strip()),
                "enabled": settings.enabled,
                "api_enabled": settings.api_enabled,
                "daily_quota_limit": settings.daily_quota_limit,
            }
            self._save_config(config)
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to save YouTube connection settings: {e}")
            raise ConfigurationError(f"Failed to save YouTube connection settings: {e}")

    def get_connect_apps_settings(self) -> ConnectAppsSettings:
        return self._get_section("connect_apps", ConnectAppsSettings)

    def save_connect_apps_settings(self, settings: ConnectAppsSettings) -> None:
        try:
            self._save_section("connect_apps", settings)
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to save Connect Apps settings: {e}")
            raise ConfigurationError(f"Failed to save Connect Apps settings: {e}")

    def get_wrapped_settings(self) -> WrappedSettings:
        config = self._load_config()
        data = config.get("wrapped_settings", {})
        return WrappedSettings(
            api_key=self._read_secret(
                ("wrapped_settings", "api_key"), data.get("api_key", "")
            ),
        )

    def save_wrapped_settings(self, settings: WrappedSettings) -> None:
        try:
            current = self.get_wrapped_settings()
            api_key = settings.api_key
            if api_key.startswith(WRAPPED_API_KEY_MASK):
                api_key = current.api_key
            else:
                api_key = api_key.strip()
            self._save_section(
                "wrapped_settings", WrappedSettings(api_key=encrypt(api_key))
            )
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to save Wrapped settings: {e}")
            raise ConfigurationError(f"Failed to save Wrapped settings.")

    def get_local_files_connection(self) -> LocalFilesConnectionSettings:
        return self._get_section("local_files_settings", LocalFilesConnectionSettings)

    def save_local_files_connection(
        self, settings: LocalFilesConnectionSettings
    ) -> None:
        try:
            self._save_section("local_files_settings", settings)
        except Exception as e:  # noqa: BLE001
            logger.error("Failed to save local files settings: %s", e)
            raise ConfigurationError(f"Failed to save local files settings: {e}")

    def get_lastfm_connection(self) -> LastFmConnectionSettings:
        config = self._load_config()
        data = config.get("lastfm_settings", {})
        return LastFmConnectionSettings(
            api_key=self._read_secret(
                ("lastfm_settings", "api_key"), data.get("api_key", "")
            ),
            shared_secret=self._read_secret(
                ("lastfm_settings", "shared_secret"), data.get("shared_secret", "")
            ),
            session_key=self._read_secret(
                ("lastfm_settings", "session_key"), data.get("session_key", "")
            ),
            username=data.get("username", ""),
            enabled=data.get("enabled", False),
        )

    def save_lastfm_connection(self, settings: LastFmConnectionSettings) -> None:
        try:
            current = self.get_lastfm_connection()

            api_key = settings.api_key.strip()
            shared_secret = settings.shared_secret
            if shared_secret.startswith(LASTFM_SECRET_MASK):
                shared_secret = current.shared_secret
            else:
                shared_secret = shared_secret.strip()

            session_key = settings.session_key
            if session_key.startswith(LASTFM_SECRET_MASK):
                session_key = current.session_key
            else:
                session_key = session_key.strip()

            username = settings.username.strip()
            enabled = settings.enabled
            if not api_key or not shared_secret:
                enabled = False
                session_key = ""
                username = ""

            resolved = LastFmConnectionSettings(
                api_key=encrypt(api_key),
                shared_secret=encrypt(shared_secret),
                session_key=encrypt(session_key),
                username=username,
                enabled=enabled,
            )
            self._save_section("lastfm_settings", resolved)
        except Exception as e:  # noqa: BLE001
            logger.error("Failed to save Last.fm connection settings: %s", e)
            raise ConfigurationError(f"Failed to save Last.fm connection settings: {e}")

    def is_lastfm_enabled(self) -> bool:
        settings = self.get_lastfm_connection()
        return (
            settings.enabled and bool(settings.api_key) and bool(settings.shared_secret)
        )

    def get_spotify_settings(self) -> SpotifySettings:
        config = self._load_config()
        data = config.get("spotify_settings", {})
        client_secret = self._read_secret(
            ("spotify_settings", "client_secret"), data.get("client_secret", "")
        )
        return SpotifySettings(
            client_id=data.get("client_id", ""),
            client_secret=SPOTIFY_SECRET_MASK if client_secret else "",
            enabled=data.get("enabled", False),
            spotify_redirect_origin=data.get("spotify_redirect_origin", ""),
        )

    def get_spotify_settings_raw(self) -> SpotifySettings:
        config = self._load_config()
        data = config.get("spotify_settings", {})
        client_secret = self._read_secret(
            ("spotify_settings", "client_secret"), data.get("client_secret", "")
        )
        return SpotifySettings(
            client_id=data.get("client_id", ""),
            client_secret=client_secret,
            enabled=data.get("enabled", False),
            spotify_redirect_origin=data.get("spotify_redirect_origin", ""),
        )

    def save_spotify_settings(self, settings: SpotifySettings) -> None:
        # GH-298: an explicit redirect origin must be a bare http(s) origin -
        # a path/query/fragment here would silently corrupt the value admins
        # register in the Spotify dashboard. Empty string = dynamic fallback.
        origin = settings.spotify_redirect_origin.strip()
        if origin:
            parts = urlsplit(origin)
            if (
                parts.scheme not in ("http", "https")
                or not parts.netloc
                or parts.path not in ("", "/")
                or parts.query
                or parts.fragment
            ):
                raise ValidationError(
                    "Spotify redirect origin must be an absolute http(s) URL"
                    " with no path, query, or fragment"
                )
        try:
            current_raw = self.get_spotify_settings_raw()
            client_secret = settings.client_secret
            if client_secret == SPOTIFY_SECRET_MASK:
                client_secret = current_raw.client_secret
            config = self._load_config().copy()
            config["spotify_settings"] = {
                "client_id": settings.client_id.strip(),
                "client_secret": encrypt(client_secret) if client_secret else "",
                "enabled": settings.enabled,
                "spotify_redirect_origin": origin.rstrip("/"),
            }
            self._save_config(config)
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to save Spotify settings: {e}")
            raise ConfigurationError("Failed to save Spotify settings")

    def is_spotify_enabled(self) -> bool:
        raw = self.get_spotify_settings_raw()
        return raw.enabled and bool(raw.client_id) and bool(raw.client_secret)

    def spotify_redirect_uri(self, request_base_url: str) -> str:
        """Build the Spotify OAuth redirect_uri - the single source of truth (GH-298).

        OAuth requires the authorize-time and token-exchange values to match
        byte-for-byte what is registered in the Spotify dashboard, so the
        authorize route, the callback's token exchange, and the admin display
        endpoint all derive through this one method. An admin-configured
        ``spotify_redirect_origin`` wins; empty keeps the historical
        request-derived base (which collapses to localhost behind untrusted
        proxies). The deployment base path (``Settings.base_path``, always
        canonical via its validator) goes between the origin and the callback
        path, exactly once - an effective origin that already ends with it
        (the mounted app's request-derived base, or a stored origin baked in
        with the prefix) is not doubled.
        """
        origin = self.get_spotify_settings_raw().spotify_redirect_origin.strip()
        base = (origin or request_base_url).rstrip("/")
        prefix = self._settings.base_path
        if prefix and not base.endswith(prefix):
            base += prefix
        return base + SPOTIFY_CALLBACK_PATH

    def with_base_path(self, target_path: str) -> str:
        """Prefix an app-relative browser redirect target with the base path.

        Same-site redirects (profile success/error pages after OAuth flows)
        must stay relative so browsers resolve them against the page they are
        issued from, while still carrying the deployment base path once.
        ``target_path`` must start with "/" and must never already include it.
        """
        return self._settings.base_path + target_path

    def get_get_it_settings(self) -> GetItSettings:
        """Return the regional storefront used by purchase-link fallbacks."""
        data = self._load_config().get("get_it", {})
        return GetItSettings(
            store_region=(data.get("store_region") or "US").upper(),
        )

    def save_get_it_settings(self, settings: GetItSettings) -> None:
        try:
            config = self._load_config().copy()
            config["get_it"] = {
                "store_region": settings.store_region.upper(),
            }
            self._save_config(config)
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to save Get it settings: {e}")
            raise ConfigurationError("Failed to save Get it settings")

    def _normalize_get_it_settings(self) -> None:
        """Remove fields retired from the purchase-link settings section."""
        config = self._load_config()
        data = config.get("get_it")
        if not isinstance(data, dict):
            return

        region = data.get("store_region")
        normalized = {
            "store_region": region.upper()
            if isinstance(region, str) and region
            else "US"
        }
        if data == normalized:
            return

        updated = config.copy()
        updated["get_it"] = normalized
        self._save_config(updated)

    def get_plugin_config(self, plugin_name: str) -> PluginConfig:
        """Per-plugin admin state (01b). Unknown plugins get the safe default:
        disabled, no settings."""
        section = self._load_config().get("plugins", {})
        data = section.get(plugin_name, {}) if isinstance(section, dict) else {}
        raw_settings = data.get("settings", {})
        settings = (
            {str(k): str(v) for k, v in raw_settings.items()}
            if isinstance(raw_settings, dict)
            else {}
        )
        return PluginConfig(enabled=bool(data.get("enabled", False)), settings=settings)

    def save_plugin_config(self, plugin_name: str, plugin_config: PluginConfig) -> None:
        try:
            config = self._load_config().copy()
            section = dict(config.get("plugins", {}))
            section[plugin_name] = {
                "enabled": plugin_config.enabled,
                "settings": dict(plugin_config.settings),
            }
            config["plugins"] = section
            self._save_config(config)
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to save plugin config for {plugin_name}: {e}")
            raise ConfigurationError("Failed to save plugin settings")

    def _events_section_raw(self) -> EventsSettings:
        config = self._load_config()
        data = config.get("events", {})
        return EventsSettings(
            enabled=data.get("enabled", False),
            ticketmaster_enabled=data.get("ticketmaster_enabled", False),
            ticketmaster_api_key=self._read_secret(
                ("events", "ticketmaster_api_key"), data.get("ticketmaster_api_key", "")
            ),
            skiddle_enabled=data.get("skiddle_enabled", False),
            skiddle_api_key=self._read_secret(
                ("events", "skiddle_api_key"), data.get("skiddle_api_key", "")
            ),
            poll_time=data.get("poll_time", "06:00"),
            sweep_scope=(
                data.get("sweep_scope")
                if data.get("sweep_scope") in ("followed", "library")
                else "followed"
            ),
        )

    def get_events_settings(self) -> EventsSettings:
        """Events settings with both API keys MASKED (safe for API responses)."""
        raw = self._events_section_raw()
        return EventsSettings(
            enabled=raw.enabled,
            ticketmaster_enabled=raw.ticketmaster_enabled,
            ticketmaster_api_key=TICKETMASTER_KEY_MASK
            if raw.ticketmaster_api_key
            else "",
            skiddle_enabled=raw.skiddle_enabled,
            skiddle_api_key=SKIDDLE_KEY_MASK if raw.skiddle_api_key else "",
            poll_time=raw.poll_time,
            sweep_scope=raw.sweep_scope,
        )

    def get_events_settings_raw(self) -> EventsSettings:
        """Events settings with both API keys DECRYPTED (server-side use only)."""
        return self._events_section_raw()

    def save_events_settings(self, settings: EventsSettings) -> None:
        try:
            current_raw = self._events_section_raw()
            tm_key = settings.ticketmaster_api_key.strip()
            if tm_key == TICKETMASTER_KEY_MASK:
                tm_key = current_raw.ticketmaster_api_key
            sk_key = settings.skiddle_api_key.strip()
            if sk_key == SKIDDLE_KEY_MASK:
                sk_key = current_raw.skiddle_api_key
            config = self._load_config().copy()
            config["events"] = {
                "enabled": settings.enabled,
                "ticketmaster_enabled": settings.ticketmaster_enabled,
                "ticketmaster_api_key": encrypt(tm_key) if tm_key else "",
                "skiddle_enabled": settings.skiddle_enabled,
                "skiddle_api_key": encrypt(sk_key) if sk_key else "",
                "poll_time": settings.poll_time,
                "sweep_scope": settings.sweep_scope,
            }
            self._save_config(config)
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to save events settings: {e}")
            raise ConfigurationError("Failed to save events settings")

    def is_events_source_ready(self) -> bool:
        """Single source of truth for 'can this instance fetch live events':
        the feature is on AND at least one source is enabled with a key."""
        raw = self._events_section_raw()
        if not raw.enabled:
            return False
        return (raw.ticketmaster_enabled and bool(raw.ticketmaster_api_key)) or (
            raw.skiddle_enabled and bool(raw.skiddle_api_key)
        )

    @staticmethod
    def _library_root_label(path: str, used: set[str]) -> str:
        base = Path(path).name or Path(path).anchor or "Library"
        label = base
        number = 2
        while label.casefold() in used:
            label = f"{base} ({number})"
            number += 1
        used.add(label.casefold())
        return label

    def _typed_library_settings_section(self) -> TypedLibrarySettings:
        """Return the canonical typed shape, migrating legacy paths once."""
        config = self._load_config()
        data = config.get("library_settings") or {}
        if "library_roots" in data:
            try:
                settings = msgspec.convert(data, type=TypedLibrarySettings)
                from services.native.library_policy_resolver import (
                    LibraryPolicyResolver,
                )

                return LibraryPolicyResolver(settings).settings
            except Exception as e:  # noqa: BLE001
                logger.error("Failed to parse typed library settings: %s", e)
                raise ConfigurationError("Library settings are invalid.") from e

        legacy_paths = list(data.get("library_paths") or [])
        if not legacy_paths:
            legacy_root = config.get("_legacy_lidarr", {}).get("root_folder_path")
            legacy_paths = [legacy_root] if legacy_root else ["/music"]

        used_labels: set[str] = set()
        roots = [
            LibraryRootSettings(
                id=str(uuid.uuid4()),
                path=path,
                label=self._library_root_label(path, used_labels),
                policy="automatic",
                rules=[],
            )
            for path in legacy_paths
        ]
        migrated = TypedLibrarySettings(
            library_roots=roots,
            staging_path=data.get("staging_path", ""),
            naming_template=data.get("naming_template") or DEFAULT_NAMING_TEMPLATE,
            acoustid_api_key=data.get("acoustid_api_key", ""),
        )
        from services.native.library_policy_resolver import LibraryPolicyResolver

        normalized = LibraryPolicyResolver(migrated).settings
        new_config = config.copy()
        new_config["library_settings"] = to_jsonable(normalized)
        self._save_config(new_config)
        return normalized

    def _library_settings_section(self) -> LibrarySettings:
        settings = self._typed_library_settings_section()
        return LibrarySettings(
            library_paths=self.get_legacy_library_paths(),
            staging_path=settings.staging_path,
            naming_template=settings.naming_template,
            acoustid_api_key=settings.acoustid_api_key,
        )

    def _library_management_settings_section(self) -> LibraryManagementSettings:
        """Return typed management policy, seeding inert presets exactly once."""

        with self._cache_lock:
            config = self._load_config()
            data = config.get("library_management")
            if data:
                try:
                    parsed = msgspec.convert(data, type=LibraryManagementSettings)
                    migrated = migrate_library_management_presets(parsed)
                    normalized = normalize_library_management_settings(migrated)
                except (
                    msgspec.ValidationError,
                    ScriptValidationError,
                    TypeError,
                    ValueError,
                ) as e:
                    logger.error("Failed to parse Library Management settings: %s", e)
                    raise ConfigurationError(
                        "Library Management settings are invalid."
                    ) from e
                if to_jsonable(normalized) != data:
                    new_config = config.copy()
                    new_config["library_management"] = to_jsonable(normalized)
                    self._save_config(new_config)
                return normalized

            legacy_template = self._typed_library_settings_section().naming_template
            migrated = build_initial_library_management_settings(legacy_template)
            new_config = self._load_config().copy()
            new_config["library_management"] = to_jsonable(migrated)
            self._save_config(new_config)
            return migrated

    def get_library_management_settings(
        self,
    ) -> LibraryManagementSettingsResponse:
        return to_library_management_response(
            self._library_management_settings_section()
        )

    def get_library_management_settings_raw(self) -> LibraryManagementSettings:
        """Server-side typed settings; the section deliberately contains no secrets."""

        stored = self._library_management_settings_section()
        return msgspec.convert(
            msgspec.to_builtins(stored), type=LibraryManagementSettings
        )

    def save_library_management_settings_if_current(
        self,
        settings: LibraryManagementSettings,
        *,
        expected_settings_revision: str,
    ) -> LibraryManagementSettingsResponse:
        """Persist policy with CAS. Saving alone never assigns or starts work."""

        with self._cache_lock:
            current = self._library_management_settings_section()
            current_revision = library_management_settings_revision(current)
            if current_revision != expected_settings_revision:
                raise StaleRevisionError(
                    "Library Management settings changed. Refresh this page and try again."
                )
            try:
                settings.preset_catalog_version = current.preset_catalog_version
                normalized = normalize_library_management_settings(settings)
                self._save_section("library_management", normalized)
            except (ScriptValidationError, ValueError) as e:
                raise ConfigurationError(str(e)) from e
            return to_library_management_response(normalized)

    def get_legacy_library_paths(self) -> list[str]:
        """Derived compatibility projection for consumers not switched to roots yet."""
        return [
            root.path for root in self._typed_library_settings_section().library_roots
        ]

    def get_typed_library_settings(self) -> TypedLibrarySettings:
        settings = self._typed_library_settings_section()
        return TypedLibrarySettings(
            library_roots=settings.library_roots,
            staging_path=settings.staging_path,
            naming_template=settings.naming_template,
            acoustid_api_key=ACOUSTID_KEY_MASK if settings.acoustid_api_key else "",
            enabled=settings.enabled,
        )

    def get_typed_library_settings_raw(self) -> TypedLibrarySettings:
        settings = self._typed_library_settings_section()
        api_key = self._read_secret(
            ("library_settings", "acoustid_api_key"), settings.acoustid_api_key
        )
        return TypedLibrarySettings(
            library_roots=settings.library_roots,
            staging_path=settings.staging_path,
            naming_template=settings.naming_template,
            acoustid_api_key=api_key,
            enabled=settings.enabled,
        )

    def get_library_settings(self) -> LibrarySettings:
        """Library settings with the AcoustID key MASKED (safe for API responses)."""
        settings = self._library_settings_section()
        return LibrarySettings(
            library_paths=settings.library_paths,
            staging_path=settings.staging_path,
            naming_template=settings.naming_template,
            acoustid_api_key=ACOUSTID_KEY_MASK if settings.acoustid_api_key else "",
        )

    def get_library_settings_raw(self) -> LibrarySettings:
        """Library settings with the AcoustID key DECRYPTED (server-side use only)."""
        settings = self._library_settings_section()
        api_key = self._read_secret(
            ("library_settings", "acoustid_api_key"), settings.acoustid_api_key
        )
        return LibrarySettings(
            library_paths=settings.library_paths,
            staging_path=settings.staging_path,
            naming_template=settings.naming_template,
            acoustid_api_key=api_key,
        )

    def save_library_settings(self, settings: LibrarySettings) -> None:
        current = self._typed_library_settings_section()
        current_by_path = {
            Path(root.path).resolve(strict=False): root
            for root in current.library_roots
        }
        used_labels = {root.label.casefold() for root in current.library_roots}
        roots: list[LibraryRootSettings] = []
        for path in settings.library_paths:
            candidate = Path(path)
            existing = (
                current_by_path.get(candidate.resolve(strict=False))
                if candidate.is_absolute()
                else None
            )
            if existing is not None:
                roots.append(existing)
                continue
            roots.append(
                LibraryRootSettings(
                    id=str(uuid.uuid4()),
                    path=path,
                    label=self._library_root_label(path, used_labels),
                    policy="automatic",
                    rules=[],
                )
            )
        self.save_typed_library_settings(
            TypedLibrarySettings(
                library_roots=roots,
                staging_path=settings.staging_path,
                naming_template=settings.naming_template,
                acoustid_api_key=settings.acoustid_api_key,
            )
        )

    def save_typed_library_settings(self, settings: TypedLibrarySettings) -> None:
        with self._cache_lock:
            self._save_typed_library_settings(settings)

    def retarget_library_roots_for_upgrade(self, replacements: dict[str, str]) -> None:
        """Retarget existing root IDs during the isolated legacy upgrade only."""

        with self._cache_lock:
            current = self.get_typed_library_settings()
            known_ids = {root.id for root in current.library_roots}
            if not replacements or not set(replacements).issubset(known_ids):
                raise ConfigurationError(
                    "The automatic upgrade supplied an unknown library root."
                )
            roots = [
                LibraryRootSettings(
                    id=root.id,
                    path=replacements.get(root.id, root.path),
                    label=root.label,
                    policy=root.policy,
                    rules=root.rules,
                )
                for root in current.library_roots
            ]
            self._save_typed_library_settings(
                TypedLibrarySettings(
                    library_roots=roots,
                    staging_path=current.staging_path,
                    naming_template=current.naming_template,
                    acoustid_api_key=current.acoustid_api_key,
                    enabled=current.enabled,
                ),
                allow_root_path_changes=True,
            )

    def save_typed_library_settings_if_current(
        self,
        settings: TypedLibrarySettings,
        *,
        expected_policy_revision: str,
    ) -> None:
        from services.native.library_policy_resolver import LibraryPolicyResolver

        with self._cache_lock:
            current = LibraryPolicyResolver(
                self._typed_library_settings_section()
            ).policy_revision
            if current != expected_policy_revision:
                raise StaleRevisionError(
                    "The library settings changed. Refresh this page and try again."
                )
            self._save_typed_library_settings(settings)

    def _save_typed_library_settings(
        self,
        settings: TypedLibrarySettings,
        *,
        allow_root_path_changes: bool = False,
    ) -> None:
        try:
            from services.native.library_policy_resolver import LibraryPolicyResolver

            current_settings = self._typed_library_settings_section()
            current_by_id = {root.id: root for root in current_settings.library_roots}
            for root in settings.library_roots:
                previous = current_by_id.get(root.id)
                if (
                    not allow_root_path_changes
                    and previous is not None
                    and Path(previous.path).resolve(strict=False)
                    != Path(root.path).resolve(strict=False)
                ):
                    raise ConfigurationError(
                        f"Library root {previous.label} cannot be moved. Add a new root instead."
                    )
                previous_rules = (
                    {rule.id: rule for rule in previous.rules}
                    if previous is not None
                    else {}
                )
                for rule in root.rules:
                    old_rule = previous_rules.get(rule.id)
                    if (
                        old_rule is not None
                        and old_rule.relative_path != rule.relative_path
                    ):
                        raise ConfigurationError(
                            "A policy rule path cannot be changed. Add a new rule instead."
                        )

            normalized = LibraryPolicyResolver(settings).settings
            current = self._load_config().get("library_settings", {})
            api_key = settings.acoustid_api_key
            if api_key == ACOUSTID_KEY_MASK:
                api_key = current.get(
                    "acoustid_api_key", ""
                )  # preserve existing (encrypted)
            elif api_key:
                api_key = encrypt(api_key)
            self._save_section(
                "library_settings",
                TypedLibrarySettings(
                    library_roots=normalized.library_roots,
                    staging_path=normalized.staging_path,
                    naming_template=normalized.naming_template
                    or DEFAULT_NAMING_TEMPLATE,
                    acoustid_api_key=api_key,
                    enabled=normalized.enabled,
                ),
            )
        except ConfigurationError:
            raise
        except Exception as e:  # noqa: BLE001
            logger.error("Failed to save library settings: %s", e)
            raise ConfigurationError(f"Failed to save library settings: {e}")

    def get_scrobble_settings(self) -> ScrobbleSettings:
        return self._get_section("scrobble_settings", ScrobbleSettings)

    def save_scrobble_settings(self, settings: ScrobbleSettings) -> None:
        try:
            self._save_section("scrobble_settings", settings)
        except Exception as e:  # noqa: BLE001
            logger.error("Failed to save scrobble settings: %s", e)
            raise ConfigurationError(f"Failed to save scrobble settings: {e}")

    def get_primary_music_source(self) -> PrimaryMusicSourceSettings:
        return self._get_section("primary_music_source", PrimaryMusicSourceSettings)

    def save_primary_music_source(self, settings: PrimaryMusicSourceSettings) -> None:
        try:
            self._save_section("primary_music_source", settings)
        except Exception as e:  # noqa: BLE001
            logger.error("Failed to save primary music source: %s", e)
            raise ConfigurationError(f"Failed to save primary music source: {e}")

    def get_setting(self, key: str) -> Any:
        config = self._load_config()
        internal = config.get("_internal", {})
        return internal.get(key)

    def save_setting(self, key: str, value: Any) -> None:
        config = self._load_config().copy()
        internal = config.get("_internal", {}).copy()
        if value is None:
            internal.pop(key, None)
        else:
            internal[key] = value
        config["_internal"] = internal
        self._save_config(config)

    def get_or_create_setting(self, key: str, factory: Any) -> Any:
        """Atomically get or create an internal setting under the cache lock."""
        with self._cache_lock:
            config = self._load_config()
            internal = config.get("_internal", {})
            value = internal.get(key)
            if value:
                return value
            value = factory() if callable(factory) else factory
            config = config.copy()
            internal = internal.copy()
            internal[key] = value
            config["_internal"] = internal
            self._save_config(config)
            return value

    def _new_brainzmash_pending(self, generation: int = 1) -> BrainzMashPendingProposal:
        return BrainzMashPendingProposal(
            endpoint=BRAINZMASH_ENDPOINT,
            access_revision=str(uuid.uuid4()),
            source_id=str(uuid.uuid4()),
            generation=max(1, generation),
            disclosure_version=BRAINZMASH_DISCLOSURE_VERSION,
        )

    @staticmethod
    def _binding_matches(
        pending: BrainzMashPendingProposal | None,
        binding: MusicBrainzBindingRequest,
    ) -> bool:
        return bool(
            PreferencesService._pending_brainzmash_policy_is_current(pending)
            and pending is not None
            and pending.access_revision == binding.access_revision
            and pending.source_id == binding.source_id
            and pending.generation == binding.generation
            and pending.disclosure_version == binding.disclosure_version
        )

    @staticmethod
    def _pending_brainzmash_policy_is_current(
        pending: BrainzMashPendingProposal | None,
    ) -> bool:
        if pending is None:
            return False

        def exact_nonblank(value: str) -> bool:
            return bool(value) and value == value.strip()

        return bool(
            pending.endpoint == BRAINZMASH_ENDPOINT
            and exact_nonblank(pending.access_revision)
            and exact_nonblank(pending.source_id)
            and pending.generation > 0
            and exact_nonblank(pending.disclosure_version)
            and pending.disclosure_version == BRAINZMASH_DISCLOSURE_VERSION
        )

    @staticmethod
    def _custom_musicbrainz_mode(raw_existing: object) -> str | None:
        """Return the explicitly configured custom mode, if it is genuine."""
        if not isinstance(raw_existing, dict):
            return None
        mode = raw_existing.get("source_mode")
        if mode not in {"mirror", "community", None, ""}:
            return None
        api_url = raw_existing.get("api_url")
        if not isinstance(api_url, str) or not api_url.strip():
            return None
        try:
            parsed = urlsplit(api_url.strip())
            hostname = parsed.hostname
        except (AttributeError, TypeError, ValueError):
            return None
        if (
            parsed.scheme.lower() not in {"http", "https"}
            or not hostname
            or hostname.casefold() == "api.brainzmash.cc"
        ):
            return None
        from repositories.musicbrainz_base import is_mb_rate_policy_public_host

        if is_mb_rate_policy_public_host(api_url):
            return None
        return mode or "mirror"

    def _default_musicbrainz_settings(
        self, raw_existing: object | None = None
    ) -> MusicBrainzConnectionSettings:
        """Build the one effective default for absent or non-custom sources."""
        source_id = ""
        generation = 1
        if (
            isinstance(raw_existing, dict)
            and raw_existing.get("source_mode") == "brainzmash"
        ):
            candidate_id = raw_existing.get("source_id")
            if (
                isinstance(candidate_id, str)
                and candidate_id
                and candidate_id == candidate_id.strip()
            ):
                source_id = candidate_id
            try:
                generation = max(1, int(raw_existing.get("generation", 1)))
            except (TypeError, ValueError):
                generation = 1
        return MusicBrainzConnectionSettings(
            source_mode="brainzmash",
            api_url=BRAINZMASH_ENDPOINT,
            rate_limit=_BRAINZMASH_RATE_LIMIT,
            concurrent_searches=1,
            selected_source_mode="brainzmash",
            source_id=source_id or str(uuid.uuid4()),
            generation=generation,
            pending_brainzmash=None,
            active_brainzmash=None,
            source_quarantined=False,
            quarantine_reason="",
        )

    def get_musicbrainz_connection(self) -> MusicBrainzConnectionSettings:
        settings = self._get_section(
            "musicbrainz_settings",
            MusicBrainzConnectionSettings,
            default_factory=self._default_musicbrainz_settings,
        )
        if settings.pending_brainzmash is None:
            settings.selected_source_mode = settings.source_mode
        return settings

    def _save_musicbrainz_connection_unlocked(
        self, settings: MusicBrainzConnectionSettings
    ) -> None:
        settings.api_url = settings.api_url.rstrip("/")
        config = self._load_config().copy()
        config["musicbrainz_settings"] = to_jsonable(settings)
        internal = config.get("_internal", {}).copy()
        if settings.source_mode == "official":
            internal["official_source_selected"] = True
        else:
            internal.pop("official_source_selected", None)
        try:
            revision = int(internal.get("musicbrainz_settings_revision", 0))
        except (TypeError, ValueError):
            revision = 0
        internal["musicbrainz_settings_revision"] = revision + 1
        config["_internal"] = internal
        self._save_config(config)

    def save_musicbrainz_connection(
        self, settings: MusicBrainzConnectionSettings
    ) -> None:
        try:
            with self._section_save_lock:
                if settings.source_mode == "official":
                    from api.v1.schemas.settings import (
                        _OFFICIAL_MB_CONCURRENT_SEARCHES,
                        _OFFICIAL_MB_RATE_LIMIT,
                    )
                    from repositories.musicbrainz_base import OFFICIAL_MB_API_BASE

                    settings.api_url = OFFICIAL_MB_API_BASE
                    settings.rate_limit = _OFFICIAL_MB_RATE_LIMIT
                    settings.concurrent_searches = _OFFICIAL_MB_CONCURRENT_SEARCHES
                    settings.selected_source_mode = "official"
                    settings.pending_brainzmash = None
                    settings.active_brainzmash = None
                    settings.source_quarantined = False
                    settings.quarantine_reason = ""
                    settings.community_acknowledged = False
                    settings.clamped_to_official_limits = False
                    if (
                        not settings.source_id
                        or settings.source_id != settings.source_id.strip()
                    ):
                        settings.source_id = str(uuid.uuid4())
                    settings.generation = max(1, settings.generation)
                elif settings.source_mode == "brainzmash":
                    if (
                        not settings.source_id
                        or settings.source_id != settings.source_id.strip()
                    ):
                        settings.source_id = str(uuid.uuid4())
                    settings.generation = max(1, settings.generation)
                    settings.selected_source_mode = "brainzmash"
                    settings.source_quarantined = False
                    settings.quarantine_reason = ""
                elif (
                    self._custom_musicbrainz_mode(
                        {
                            "source_mode": settings.source_mode,
                            "api_url": settings.api_url,
                        }
                    )
                    != settings.source_mode
                ):
                    settings = self._default_musicbrainz_settings()
                self._save_musicbrainz_connection_unlocked(settings)
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to save MusicBrainz settings: {e}")
            raise ConfigurationError(f"Failed to save MusicBrainz settings: {e}")

    @staticmethod
    def _musicbrainz_settings_equal(
        left: MusicBrainzConnectionSettings,
        right: MusicBrainzConnectionSettings,
    ) -> bool:
        return to_jsonable(left) == to_jsonable(right)

    def musicbrainz_settings_revision(self) -> int:
        """Return the persisted source revision under the section lock."""
        with self._section_save_lock:
            internal = self._load_config().get("_internal", {})
            try:
                return int(internal.get("musicbrainz_settings_revision", 0))
            except (TypeError, ValueError):
                return 0

    def get_musicbrainz_settings_revision(self) -> int:
        return self.musicbrainz_settings_revision()

    def musicbrainz_settings_match(
        self, expected: MusicBrainzConnectionSettings
    ) -> bool:
        """Compare the current source section under the section transaction lock."""
        with self._section_save_lock:
            return self._musicbrainz_settings_equal(
                self._get_section(
                    "musicbrainz_settings",
                    MusicBrainzConnectionSettings,
                    default_factory=self._default_musicbrainz_settings,
                ),
                expected,
            )

    def restore_musicbrainz_connection_if_current(
        self,
        expected: MusicBrainzConnectionSettings,
        replacement: MusicBrainzConnectionSettings,
        *,
        expected_revision: int | None = None,
    ) -> bool:
        """CAS-restore a failed runtime commit without clobbering newer settings."""
        with self._section_save_lock:
            if expected_revision is not None:
                internal = self._load_config().get("_internal", {})
                try:
                    revision = int(internal.get("musicbrainz_settings_revision", 0))
                except (TypeError, ValueError):
                    revision = 0
                if revision != expected_revision:
                    return False
            current = self._get_section(
                "musicbrainz_settings",
                MusicBrainzConnectionSettings,
                default_factory=self._default_musicbrainz_settings,
            )
            if not self._musicbrainz_settings_equal(current, expected):
                return False
            self._save_musicbrainz_connection_unlocked(replacement)
            return True

    def save_musicbrainz_update(
        self, update: MusicBrainzSettingsUpdate
    ) -> MusicBrainzConnectionSettings:
        if update.source_mode == "official":
            return self._save_deliberate_official_update()
        brainzmash_update = update.source_mode == "brainzmash"
        if not brainzmash_update:
            api_url = (update.api_url or "").strip().rstrip("/")
            brainzmash_update = (
                self._custom_musicbrainz_mode(
                    {"source_mode": update.source_mode, "api_url": api_url}
                )
                != update.source_mode
            )

        if (
            not brainzmash_update
            and update.source_mode == "community"
            and not update.community_acknowledged
        ):
            raise ConfigurationError("Community source acknowledgement is required")

        with self._section_save_lock:
            previous = self.get_musicbrainz_connection()
            if brainzmash_update:
                source_changed = previous.source_mode != "brainzmash"
                source_id = (
                    str(uuid.uuid4())
                    if source_changed or not previous.source_id.strip()
                    else previous.source_id
                )
                generation = (
                    max(1, previous.generation) + 1
                    if source_changed
                    else max(1, previous.generation)
                )
                settings = MusicBrainzConnectionSettings(
                    source_mode="brainzmash",
                    api_url=BRAINZMASH_ENDPOINT,
                    rate_limit=_BRAINZMASH_RATE_LIMIT,
                    concurrent_searches=1,
                    selected_source_mode="brainzmash",
                    source_id=source_id,
                    generation=generation,
                    active_brainzmash=(
                        previous.active_brainzmash if not source_changed else None
                    ),
                    pending_brainzmash=None,
                    source_quarantined=False,
                    quarantine_reason="",
                )
                self._save_musicbrainz_connection_unlocked(settings)
                return settings

            api_url = (update.api_url or "").strip().rstrip("/")
            source_changed = (
                previous.source_mode != update.source_mode
                or previous.api_url.rstrip("/") != api_url
            )
            if source_changed:
                source_id = str(uuid.uuid4())
                generation = max(1, previous.generation) + 1
            else:
                source_id = previous.source_id
                generation = max(1, previous.generation)
            settings = MusicBrainzConnectionSettings(
                source_mode=update.source_mode,
                api_url=api_url,
                rate_limit=update.rate_limit,
                concurrent_searches=update.concurrent_searches,
                community_acknowledged=bool(update.community_acknowledged),
                selected_source_mode=update.source_mode,
                source_id=source_id,
                generation=generation,
            )
            self._save_musicbrainz_connection_unlocked(settings)
            return settings

    def _save_deliberate_official_update(self) -> MusicBrainzConnectionSettings:
        """Persist an admin-chosen Official source as a genuine override.

        The startup migration still converts upgrade-time Official sections to
        BrainzMash; this path records the internal marker that distinguishes a
        later deliberate Official save from migrated state, so the choice
        survives restarts.
        """
        from api.v1.schemas.settings import (
            _OFFICIAL_MB_CONCURRENT_SEARCHES,
            _OFFICIAL_MB_RATE_LIMIT,
        )
        from repositories.musicbrainz_base import OFFICIAL_MB_API_BASE

        with self._section_save_lock:
            previous = self.get_musicbrainz_connection()
            source_changed = previous.source_mode != "official"
            source_id = (
                str(uuid.uuid4())
                if source_changed or not previous.source_id.strip()
                else previous.source_id
            )
            generation = (
                max(1, previous.generation) + 1
                if source_changed
                else max(1, previous.generation)
            )
            settings = MusicBrainzConnectionSettings(
                source_mode="official",
                api_url=OFFICIAL_MB_API_BASE,
                rate_limit=_OFFICIAL_MB_RATE_LIMIT,
                concurrent_searches=_OFFICIAL_MB_CONCURRENT_SEARCHES,
                selected_source_mode="official",
                source_id=source_id,
                generation=generation,
            )
            self._save_musicbrainz_connection_unlocked(settings)
            return settings

    def _clear_official_source_marker(self) -> None:
        config = self._load_config().copy()
        internal = config.get("_internal", {})
        if not isinstance(internal, dict) or "official_source_selected" not in internal:
            return
        internal = internal.copy()
        internal.pop("official_source_selected", None)
        config["_internal"] = internal
        self._save_config(config)

    def stage_brainzmash(self) -> MusicBrainzConnectionSettings:
        """Select BrainzMash immediately; disclosure handling remains optional."""
        with self._section_save_lock:
            previous = self.get_musicbrainz_connection()
            source_changed = previous.source_mode != "brainzmash"
            source_id = (
                str(uuid.uuid4())
                if source_changed or not previous.source_id.strip()
                else previous.source_id
            )
            generation = (
                max(1, previous.generation) + 1
                if source_changed
                else max(1, previous.generation)
            )
            settings = MusicBrainzConnectionSettings(
                source_mode="brainzmash",
                api_url=BRAINZMASH_ENDPOINT,
                rate_limit=_BRAINZMASH_RATE_LIMIT,
                concurrent_searches=1,
                selected_source_mode="brainzmash",
                source_id=source_id,
                generation=generation,
                active_brainzmash=(
                    previous.active_brainzmash if not source_changed else None
                ),
                pending_brainzmash=self._new_brainzmash_pending(generation + 1),
                source_quarantined=False,
                quarantine_reason="",
            )
            self._save_musicbrainz_connection_unlocked(settings)
            return settings

    def accept_brainzmash_consent(
        self, binding: MusicBrainzBindingRequest, admin_id: str
    ) -> MusicBrainzConnectionSettings:
        with self._section_save_lock:
            current = self.get_musicbrainz_connection()
            if not self._binding_matches(current.pending_brainzmash, binding):
                raise ValidationError("BrainzMash proposal is stale")
            assert current.pending_brainzmash is not None
            if (
                current.pending_brainzmash.disclosure_version
                != BRAINZMASH_DISCLOSURE_VERSION
            ):
                raise ValidationError("BrainzMash disclosure is outdated")
            current.pending_brainzmash.consented = True
            self._save_musicbrainz_connection_unlocked(current)
            config = self._load_config().copy()
            config["_internal"] = {
                **config.get("_internal", {}),
                "brainzmash_consent_admin": admin_id,
            }
            self._save_config(config)
            return current

    def record_brainzmash_verification(
        self, binding: MusicBrainzBindingRequest
    ) -> MusicBrainzConnectionSettings:
        with self._section_save_lock:
            current = self.get_musicbrainz_connection()
            if not self._binding_matches(current.pending_brainzmash, binding):
                raise ValidationError("BrainzMash proposal is stale")
            assert current.pending_brainzmash is not None
            if not current.pending_brainzmash.consented:
                raise ValidationError("BrainzMash consent is required")
            current.pending_brainzmash.verified = True
            self._save_musicbrainz_connection_unlocked(current)
            return current

    def promote_brainzmash(
        self, binding: MusicBrainzBindingRequest
    ) -> tuple[MusicBrainzConnectionSettings, MusicBrainzConnectionSettings]:
        """Promote an exact verified proposal and return (previous, promoted)."""
        with self._section_save_lock:
            current = self.get_musicbrainz_connection()
            if not self._binding_matches(current.pending_brainzmash, binding):
                raise ValidationError("BrainzMash proposal is stale")
            pending = current.pending_brainzmash
            assert pending is not None
            if not pending.consented:
                raise ValidationError("BrainzMash consent is required")
            if not pending.verified:
                raise ValidationError("BrainzMash verification is required")
            previous = msgspec.convert(
                msgspec.to_builtins(current), type=MusicBrainzConnectionSettings
            )
            promoted = MusicBrainzConnectionSettings(
                source_mode="brainzmash",
                api_url=pending.endpoint.rstrip("/"),
                rate_limit=_BRAINZMASH_RATE_LIMIT,
                concurrent_searches=1,
                selected_source_mode="brainzmash",
                source_id=pending.source_id,
                generation=pending.generation,
                pending_brainzmash=None,
                active_brainzmash=BrainzMashActiveBinding(
                    endpoint=pending.endpoint,
                    access_revision=pending.access_revision,
                    source_id=pending.source_id,
                    generation=pending.generation,
                    disclosure_version=pending.disclosure_version,
                    consented=True,
                    verified=True,
                ),
            )
            self._save_musicbrainz_connection_unlocked(promoted)
            return previous, promoted

    def _legacy_custom_musicbrainz_settings(
        self, raw_existing: object, mode: str
    ) -> MusicBrainzConnectionSettings | None:
        if not isinstance(raw_existing, dict):
            return None
        api_url = raw_existing.get("api_url")
        if not isinstance(api_url, str):
            return None
        try:
            rate_limit = float(raw_existing.get("rate_limit", 1.0))
            concurrent_searches = int(raw_existing.get("concurrent_searches", 6))
            return MusicBrainzConnectionSettings(
                source_mode=mode,
                api_url=api_url,
                rate_limit=rate_limit,
                concurrent_searches=concurrent_searches,
                community_acknowledged=bool(
                    raw_existing.get("community_acknowledged", False)
                ),
                selected_source_mode=mode,
                source_id=str(raw_existing.get("source_id") or uuid.uuid4()),
                generation=max(1, int(raw_existing.get("generation", 1))),
            )
        except (TypeError, ValueError, msgspec.ValidationError):
            return None

    def _migrate_musicbrainz_settings(self) -> None:
        """Normalize every non-custom source to the built-in BrainzMash default.

        A deliberate Official re-selection recorded by
        `_save_deliberate_official_update` is preserved instead of migrated.
        """
        from api.v1.schemas.settings import (
            _OFFICIAL_MB_CONCURRENT_SEARCHES,
            _OFFICIAL_MB_RATE_LIMIT,
        )
        from repositories.musicbrainz_base import OFFICIAL_MB_API_BASE

        try:
            config = self._load_config()
            existing = config.get("musicbrainz_settings")
            internal = config.get("_internal", {})
            deliberate_official = bool(
                isinstance(internal, dict)
                and internal.get("official_source_selected") is True
            )
            custom_mode = self._custom_musicbrainz_mode(existing)
            if custom_mode is not None:
                if deliberate_official:
                    self._clear_official_source_marker()
                if isinstance(existing, dict) and existing.get("source_mode") in {
                    "mirror",
                    "community",
                }:
                    try:
                        custom = msgspec.convert(
                            existing, type=MusicBrainzConnectionSettings
                        )
                    except (msgspec.ValidationError, TypeError, ValueError):
                        self._save_section(
                            "musicbrainz_settings",
                            self._default_musicbrainz_settings(existing),
                        )
                        return
                    if (
                        custom.source_mode != custom_mode
                        or custom.selected_source_mode != custom_mode
                        or not custom.source_id
                        or custom.source_id != custom.source_id.strip()
                        or custom.generation < 1
                    ):
                        self._save_section(
                            "musicbrainz_settings",
                            self._default_musicbrainz_settings(existing),
                        )
                        return
                    # A complete valid custom section is preserved byte-for-byte.
                    return
                # A pre-source-mode custom URL is the one legacy case worth
                # preserving; its active endpoint remains the user's choice.
                migrated = self._legacy_custom_musicbrainz_settings(
                    existing, custom_mode
                )
                if migrated is not None:
                    self._save_section("musicbrainz_settings", migrated)
                else:
                    self._save_section(
                        "musicbrainz_settings",
                        self._default_musicbrainz_settings(existing),
                    )
                return

            if (
                isinstance(existing, dict)
                and existing.get("source_mode") == "brainzmash"
            ):
                try:
                    current = msgspec.convert(
                        existing, type=MusicBrainzConnectionSettings
                    )
                except (msgspec.ValidationError, TypeError, ValueError):
                    current = None
                if deliberate_official:
                    self._clear_official_source_marker()
                if current is not None:
                    changed = to_jsonable(current) != existing
                    if (
                        not current.source_id
                        or current.source_id != current.source_id.strip()
                    ):
                        current.source_id = str(uuid.uuid4())
                        changed = True
                    if current.generation < 1:
                        current.generation = 1
                        changed = True
                    if current.selected_source_mode != "brainzmash":
                        current.selected_source_mode = "brainzmash"
                        changed = True
                    if current.source_quarantined or current.quarantine_reason:
                        current.source_quarantined = False
                        current.quarantine_reason = ""
                        changed = True
                    if changed:
                        self._save_section("musicbrainz_settings", current)
                    return

            if (
                deliberate_official
                and isinstance(existing, dict)
                and existing.get("source_mode") == "official"
            ):
                # An admin re-selected Official after the upgrade migration.
                # Canonicalize the section but never convert it to BrainzMash.
                current = msgspec.convert(existing, type=MusicBrainzConnectionSettings)
                current.source_mode = "official"
                current.api_url = OFFICIAL_MB_API_BASE
                current.rate_limit = _OFFICIAL_MB_RATE_LIMIT
                current.concurrent_searches = _OFFICIAL_MB_CONCURRENT_SEARCHES
                current.selected_source_mode = "official"
                current.pending_brainzmash = None
                current.active_brainzmash = None
                current.source_quarantined = False
                current.quarantine_reason = ""
                current.community_acknowledged = False
                current.clamped_to_official_limits = False
                if (
                    not current.source_id
                    or current.source_id != current.source_id.strip()
                ):
                    current.source_id = str(uuid.uuid4())
                if current.generation < 1:
                    current.generation = 1
                if to_jsonable(current) != existing:
                    self._save_section("musicbrainz_settings", current)
                return

            if deliberate_official:
                self._clear_official_source_marker()
            # Missing, legacy official, explicit official, malformed, and
            # ambiguous states all take the same active BrainzMash path.
            self._save_section(
                "musicbrainz_settings",
                self._default_musicbrainz_settings(existing),
            )
        except Exception:  # noqa: BLE001
            logger.warning("Failed to normalize MusicBrainz settings; using BrainzMash")
            self._clear_official_source_marker()
            self._save_section(
                "musicbrainz_settings", self._default_musicbrainz_settings()
            )

    def get_oidc_connection(self) -> OIDCConnectionSettings:
        config = self._load_config()
        data = config.get("oidc_settings", {})
        return OIDCConnectionSettings(
            enabled=data.get("enabled", False),
            issuer=data.get("issuer", ""),
            client_id=data.get("client_id", ""),
            client_secret=OIDC_SECRET_MASK if data.get("client_secret") else "",
            scopes=data.get("scopes", "openid email profile"),
            redirect_uri=data.get("redirect_uri", ""),
        )

    def get_oidc_connection_raw(self) -> OIDCConnectionSettings:
        config = self._load_config()
        data = config.get("oidc_settings", {})
        secret = self._read_secret(
            ("oidc_settings", "client_secret"), data.get("client_secret", "")
        )
        return OIDCConnectionSettings(
            enabled=data.get("enabled", False),
            issuer=data.get("issuer", ""),
            client_id=data.get("client_id", ""),
            client_secret=secret,
            scopes=data.get("scopes", "openid email profile"),
            redirect_uri=data.get("redirect_uri", ""),
        )

    def save_oidc_connection(self, settings: OIDCConnectionSettings) -> None:
        try:
            config = self._load_config().copy()
            current_data = config.get("oidc_settings", {})
            secret = settings.client_secret
            if secret == OIDC_SECRET_MASK:
                secret = current_data.get("client_secret", "")
            else:
                secret = encrypt(secret)
            config["oidc_settings"] = {
                "enabled": settings.enabled,
                "issuer": settings.issuer,
                "client_id": settings.client_id,
                "client_secret": secret,
                "scopes": settings.scopes,
                "redirect_uri": settings.redirect_uri,
            }
            self._save_config(config)
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to save OIDC settings: {e}")
            raise ConfigurationError("Failed to save OIDC settings")

    def get_security_settings(self) -> SecuritySettings:
        return self._get_section("security_settings", SecuritySettings)

    def save_security_settings(self, settings: SecuritySettings) -> None:
        try:
            self._save_section("security_settings", settings)
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to save security settings: {e}")
            raise ConfigurationError("Failed to save security settings")
