from pathlib import Path
from pydantic import Field, TypeAdapter, ValidationError as PydanticValidationError, field_validator, model_validator
from pydantic_settings import BaseSettings, SettingsConfigDict
from typing import Self
import logging
import msgspec
from core.exceptions import ConfigurationError
from infrastructure.file_utils import atomic_write_json, read_json

logger = logging.getLogger(__name__)

_VALID_LOG_LEVELS = frozenset({"DEBUG", "INFO", "WARNING", "ERROR", "CRITICAL"})


class Settings(BaseSettings):
    model_config = SettingsConfigDict(
        env_file=".env",
        env_file_encoding="utf-8",
        case_sensitive=False,
        extra="allow"
    )
    
    jellyfin_url: str = Field(default="http://jellyfin:8096")

    contact_email: str = Field(
        default="contact@droppedneedle.com",
        description="Contact email for MusicBrainz API User-Agent. Override with your own if desired."
    )
    discover_warmer_enabled: bool = Field(
        default=True,
        description="Proactively warm per-user Discover/Home in the background through the day (kill switch)."
    )

    port: int = Field(default=8688)
    debug: bool = Field(default=False)
    log_level: str = Field(default="INFO")
    
    cache_ttl_default: int = Field(default=60)
    cache_ttl_artist: int = Field(default=3600)
    cache_ttl_album: int = Field(default=3600)
    cache_ttl_covers: int = Field(default=86400, description="Cover cache TTL in seconds (default: 24 hours)")
    cache_cleanup_interval: int = Field(default=300)
    
    root_app_dir: Path = Field(default=Path("/app"), description="Root application directory")
    cache_dir: Path = Field(default=Path("/app/cache"), description="Root directory for all cache files")
    library_db_path: Path = Field(default=Path("/app/cache/library.db"), description="SQLite library database path")
    cover_cache_max_size_mb: int = Field(default=500, description="Maximum cover cache size in MB")
    trusted_proxy_ips: str = Field(
        default="*",
        description="Comma-separated IPs/CIDRs trusted as reverse proxies for X-Forwarded-* headers. Use '*' to trust all (default, safe behind a single-host Docker setup). Restrict to your proxy's IP in production.",
    )
    slskd_downloads_path: Path = Field(
        default=Path("/data/downloads/slskd"),
        description="Mounted slskd downloads directory (read-write, same filesystem as the library); import source for completed downloads.",
    )
    download_client_concurrent_searches: int = Field(
        default=1, description="Max concurrent slskd searches"
    )
    download_client_concurrent_enqueues: int = Field(
        default=1, description="Max concurrent slskd enqueues (slskd permits only one - C3)"
    )
    shutdown_grace_period: float = Field(default=10.0, description="Seconds to wait for tasks on shutdown")
    
    http_timeout: float = Field(default=10.0)
    http_connect_timeout: float = Field(default=5.0)
    http_max_connections: int = Field(default=200)
    http_max_keepalive: int = Field(default=50)
    
    config_file_path: Path = Field(default=Path("/app/config/config.json"))
    audiodb_api_key: str = Field(default="123")
    audiodb_premium: bool = Field(default=False, description="Set to true if using a premium AudioDB API key")
    instance_id: str = Field(default="", description="Auto-generated per-instance UUID for User-Agent differentiation")
    
    @field_validator("log_level")
    @classmethod
    def validate_log_level(cls, v: str) -> str:
        normalised = v.upper()
        if normalised not in _VALID_LOG_LEVELS:
            raise ValueError(
                f"Invalid log_level '{v}'. Must be one of: {', '.join(sorted(_VALID_LOG_LEVELS))}"
            )
        return normalised

    @field_validator("jellyfin_url")
    @classmethod
    def validate_url(cls, v: str) -> str:
        return v.rstrip("/")

    @model_validator(mode='after')
    def validate_config(self) -> Self:
        if self.cache_dir == Path("/app/cache"):
            self.cache_dir = self.root_app_dir / "cache"
        if self.library_db_path == Path("/app/cache/library.db"):
            self.library_db_path = self.cache_dir / "library.db"
        if self.config_file_path == Path("/app/config/config.json"):
            self.config_file_path = self.root_app_dir / "config" / "config.json"

        errors = []
        warnings = []

        url = getattr(self, 'jellyfin_url', '')
        if url and not url.startswith(('http://', 'https://')):
            errors.append("jellyfin_url must start with http:// or https://")

        if self.http_max_connections < self.http_max_keepalive * 2:
            warnings.append(
                f"http_max_connections ({self.http_max_connections}) should be "
                f"at least 2x http_max_keepalive ({self.http_max_keepalive})"
            )

        for warning in warnings:
            logger.warning(warning)

        if errors:
            raise ConfigurationError(
                f"Critical configuration errors: {'; '.join(errors)}"
            )

        return self
    
    def get_user_agent(self) -> str:
        id_part = self.instance_id[:8] if self.instance_id else "unknown"
        return f"DroppedNeedle/1.0 ({id_part}; {self.contact_email}; https://www.droppedneedle.com)"

    def load_from_file(self) -> None:
        if not self.config_file_path.exists():
            self._create_default_config()
            return
        
        try:
            config_data = read_json(self.config_file_path, default={})
            if not isinstance(config_data, dict):
                raise ValueError("Config file JSON root must be an object")
            
            type_errors: list[str] = []
            model_fields = type(self).model_fields
            validated_values: dict[str, object] = {}
            for key, value in config_data.items():
                if key not in model_fields:
                    logger.warning("Unknown config key '%s', ignoring", key)
                    continue
                try:
                    field_info = model_fields[key]
                    adapter = TypeAdapter(field_info.annotation)
                    validated_values[key] = adapter.validate_python(value)
                except PydanticValidationError as e:
                    type_errors.append(
                        f"'{key}': {e.errors()[0].get('msg', str(e))}"
                    )
                except (TypeError, ValueError) as e:
                    type_errors.append(f"'{key}': {e}")

            if type_errors:
                raise ConfigurationError(
                    f"Config file type errors: {'; '.join(type_errors)}"
                )

            # field validators TypeAdapter doesn't invoke
            try:
                if 'jellyfin_url' in validated_values:
                    validated_values['jellyfin_url'] = type(self).validate_url(
                        validated_values['jellyfin_url']
                    )
                if 'log_level' in validated_values:
                    validated_values['log_level'] = type(self).validate_log_level(
                        validated_values['log_level']
                    )
            except ValueError as e:
                raise ConfigurationError(f"Config file validation error: {e}")

            # cross-field validation on merged candidate state before mutating self
            self._validate_merged(validated_values)

            for key, value in validated_values.items():
                setattr(self, key, value)

        except (ConfigurationError, ValueError):
            raise
        except msgspec.DecodeError as e:
            logger.error(f"Invalid JSON in config file: {e}")
            raise ValueError(f"Config file is not valid JSON: {e}")
        except Exception as e:
            logger.error(f"Failed to load config: {e}")
            raise

    def _validate_merged(self, overrides: dict[str, object]) -> None:
        """Cross-field validation against candidate merged state without mutating self."""
        errors = []

        def _get(field: str) -> object:
            return overrides.get(field, getattr(self, field))

        url = _get('jellyfin_url')
        if url and not str(url).startswith(('http://', 'https://')):
            errors.append("jellyfin_url must start with http:// or https://")

        if errors:
            raise ConfigurationError(
                f"Critical configuration errors: {'; '.join(errors)}"
            )
    
    def _create_default_config(self) -> None:
        self.config_file_path.parent.mkdir(parents=True, exist_ok=True)
        config_data = {
            "jellyfin_url": self.jellyfin_url,
            "contact_email": self.contact_email,
            "port": self.port,
            "audiodb_api_key": self.audiodb_api_key,
            "audiodb_premium": self.audiodb_premium,
            "user_preferences": {
                "primary_types": ["album", "ep", "single"],
                "secondary_types": ["studio"],
            },
        }
        atomic_write_json(self.config_file_path, config_data)
    
    def save_to_file(self) -> None:
        try:
            self.config_file_path.parent.mkdir(parents=True, exist_ok=True)
            
            config_data = {}
            if self.config_file_path.exists():
                loaded = read_json(self.config_file_path, default={})
                config_data = loaded if isinstance(loaded, dict) else {}
            
            config_data.update({
                "jellyfin_url": self.jellyfin_url,
                "contact_email": self.contact_email,
                "port": self.port,
                "audiodb_api_key": self.audiodb_api_key,
                "audiodb_premium": self.audiodb_premium,
            })
            
            atomic_write_json(self.config_file_path, config_data)
            
        except Exception as e:
            logger.error(f"Failed to save config: {e}")
            raise


_settings: Settings | None = None


def get_settings() -> Settings:
    global _settings
    if _settings is None:
        settings = Settings()
        settings.load_from_file()
        _settings = settings
    return _settings


_LEGACY_CONFIG_KEYS = (
    "lidarr_url",
    "lidarr_api_key",
    "lidarr_timeout",
    "quality_profile_id",
    "metadata_profile_id",
    "root_folder_path",
)


def migrate_legacy_config() -> None:
    """One-time startup migration.

    Removed Lidarr-era config keys are backed up into ``_legacy_lidarr`` (so Phase 3 can
    seed the native library paths from ``root_folder_path``) and dropped from the active config.
    Idempotent: a config with no legacy keys is left untouched.
    """
    path = get_settings().config_file_path
    if not path.exists():
        return
    try:
        config = read_json(path, default={})
    except Exception as e:  # noqa: BLE001
        logger.warning("Skipping legacy config migration (config unreadable): %s", e)
        return
    if not isinstance(config, dict) or not any(k in config for k in _LEGACY_CONFIG_KEYS):
        return
    config["_legacy_lidarr"] = {
        "url": config.get("lidarr_url"),
        "api_key": config.get("lidarr_api_key"),
        "timeout": config.get("lidarr_timeout"),
        "quality_profile_id": config.get("quality_profile_id"),
        "metadata_profile_id": config.get("metadata_profile_id"),
        "root_folder_path": config.get("root_folder_path"),
    }
    for key in _LEGACY_CONFIG_KEYS:
        config.pop(key, None)
    atomic_write_json(path, config)
    logger.info("Migrated legacy config keys into _legacy_lidarr backup")
