import re
from typing import Annotated, Literal

import msgspec

from api.v1.schemas.advanced_settings import _validate_range
from api.v1.schemas.plex import PlexLibrarySectionInfo
from infrastructure.msgspec_fastapi import AppStruct

LASTFM_SECRET_MASK = "••••••••"


def _mask_secret(value: str) -> str:
    if not value:
        return ""
    if len(value) <= 4:
        return LASTFM_SECRET_MASK
    return LASTFM_SECRET_MASK + value[-4:]


class LastFmConnectionSettings(AppStruct):
    api_key: str = ""
    shared_secret: str = ""
    session_key: str = ""
    username: str = ""
    enabled: bool = False


class LastFmConnectionSettingsResponse(AppStruct):
    api_key: str = ""
    shared_secret: str = ""
    session_key: str = ""
    username: str = ""
    enabled: bool = False

    @classmethod
    def from_settings(cls, settings: LastFmConnectionSettings) -> "LastFmConnectionSettingsResponse":
        return cls(
            api_key=settings.api_key,
            shared_secret=_mask_secret(settings.shared_secret),
            session_key=_mask_secret(settings.session_key),
            username=settings.username,
            enabled=settings.enabled,
        )


class LastFmVerifyResponse(AppStruct):
    valid: bool
    message: str


class LastFmAuthTokenResponse(AppStruct):
    token: str
    auth_url: str


class LastFmAuthSessionRequest(AppStruct):
    token: str


class LastFmAuthSessionResponse(AppStruct):
    success: bool
    message: str
    username: str = ""


class UserPreferences(AppStruct):
    primary_types: list[str] = msgspec.field(default_factory=lambda: ["album", "ep", "single"])
    secondary_types: list[str] = msgspec.field(default_factory=lambda: ["studio"])


class DownloadClientConnectionSettings(AppStruct):
    """slskd download client connection + quality/verification settings.

    api_key is a Fernet-encrypted secret, masked on read, preserved on save when
    the masked sentinel comes back unchanged.
    """
    enabled: bool = False
    client_type: str = "slskd"
    url: str = ""
    api_key: str = ""
    verify_downloads: bool = True
    min_bitrate_kbps: int = 128  # deprecated: superseded by quality_min/max
    quality_min: str = "mp3_320"
    quality_max: str = "lossless"
    flac_mp3_only: bool = True
    # Optional subfolder inside the mount where slskd actually saves completed downloads,
    # for when the mount points at a parent (e.g. the whole media share). Relative, never
    # escapes the mount (sanitised below + confined again at use).
    downloads_subpath: str = ""
    preflight_score_auto_accept: float = 0.70
    preflight_score_manual_min: float = 0.50
    # Download resilience (minutes-scale; user-tunable). A transfer actively
    # connecting/moving bytes that then freezes is a stall; a transfer still
    # sitting in the peer's remote upload queue gets the more generous timeout.
    download_stall_timeout_minutes: int = 30
    download_queued_timeout_minutes: int = 120
    max_failover_attempts: int = 3
    max_concurrent_downloads: int = 3
    auto_retry_enabled: bool = True
    auto_retry_max_attempts: int = 6
    auto_retry_base_interval_minutes: int = 15

    def __post_init__(self) -> None:
        # normalise a bare host (e.g. "slskd:5030") to a full URL; httpx rejects a
        # schemeless URL
        self.url = self.url.strip()
        if self.url and not self.url.startswith(("http://", "https://")):
            self.url = f"https://{self.url}"
        self.url = self.url.rstrip("/")
        _validate_range(
            self.download_stall_timeout_minutes, "download_stall_timeout_minutes", 2, 240
        )
        _validate_range(
            self.download_queued_timeout_minutes, "download_queued_timeout_minutes", 5, 1440
        )
        _validate_range(self.max_failover_attempts, "max_failover_attempts", 1, 10)
        _validate_range(self.max_concurrent_downloads, "max_concurrent_downloads", 1, 10)
        _validate_range(self.auto_retry_max_attempts, "auto_retry_max_attempts", 0, 20)
        _validate_range(
            self.auto_retry_base_interval_minutes, "auto_retry_base_interval_minutes", 1, 1440
        )
        # mirrors services.native.quality_tiers.TIER_KEYS (best -> worst); keep in sync
        _rank = {k: r for r, k in enumerate(("low", "mp3_192", "mp3_256", "mp3_320", "lossless"))}
        if self.quality_min not in _rank:
            self.quality_min = "mp3_320"
        if self.quality_max not in _rank:
            self.quality_max = "lossless"
        if _rank[self.quality_min] > _rank[self.quality_max]:
            self.quality_min = self.quality_max
        # keep only safe, relative path components - the subpath joins onto the mount, so
        # drop "", ".", ".." and any leading slashes so it can never escape it
        self.downloads_subpath = "/".join(
            p for p in re.split(r"[\\/]", self.downloads_subpath.strip())
            if p and p not in (".", "..")
        )


class DownloadPolicySettings(AppStruct):
    """Source-agnostic acquisition policy (review M5). Lives in its own config section
    so a Usenet-only install (slskd disabled) still has quality/threshold/timeout/retry
    settings to read - they must NOT live on the slskd client struct. The orchestrator +
    both scorers read these."""

    quality_min: str = "mp3_320"
    quality_max: str = "lossless"
    flac_mp3_only: bool = True
    verify_downloads: bool = True
    preflight_score_auto_accept: float = 0.70
    preflight_score_manual_min: float = 0.50
    download_stall_timeout_minutes: int = 30
    download_queued_timeout_minutes: int = 120
    max_failover_attempts: int = 3
    max_concurrent_downloads: int = 3
    auto_retry_enabled: bool = True
    auto_retry_max_attempts: int = 6
    auto_retry_base_interval_minutes: int = 15
    # Don't permanently blocklist a Usenet release younger than this; let it retry once it
    # has propagated across Usenet servers (review M4 / owner Q2).
    usenet_min_release_age_minutes: int = 30
    # --- Acquisition-pipeline substrate (ArrRebuild). Source-agnostic; consumed by the
    # decision specs. All default to "off" so behaviour is unchanged until opted in. ---
    # Hard upper bound on a single album's total size (0 = unbounded). Rejects a mislabeled
    # boxset/discography before the bytes are spent.
    max_size_mb: int = 0
    # Reject a Usenet release older than this many days (0 = no limit) - beyond a provider's
    # retention the articles can't be fetched, so it would only download partially and fail.
    usenet_retention_days: int = 0
    # Drop any release whose title/folder matches one of these (plain substring, or /regex/i).
    # The always-on wrong-album/wrong-edition guards stay; this is user-tunable extra control.
    ignored_terms: list[str] = msgspec.field(default_factory=list)
    # When non-empty, a release must match at least one of these to be considered.
    required_terms: list[str] = msgspec.field(default_factory=list)
    # --- Upgrade/cutoff substrate (full feature lives in CollectionManagement). ---
    # Stop upgrading once a release-group's worst track reaches this tier.
    quality_cutoff: str = "lossless"
    # Master opt-in for replacing held lower-quality files with better ones.
    upgrade_allowed: bool = False
    # Where files replaced by a quality upgrade are moved instead of deleted (D19:
    # upgrade-replacements ONLY; user-initiated deletes stay hard deletes). Empty =
    # "<first library path>/.recycle" (dot-prefixed, so the scanner skips it).
    recycle_bin_path: str = ""
    # Recycled files older than this are pruned by the periodic task.
    recycle_retention_days: int = 30
    # --- Cost control (CollectionManagement Feature C). 0 = unlimited. Caps BLOCK
    # new non-upgrade grabs at/over the ceiling; nothing is ever auto-evicted (A3).
    # Per-user values are defaults; user_quotas rows override them (NULL = inherit).
    max_library_size_gb: int = 0
    default_request_quota_count: int = 0
    default_request_quota_days: int = 7
    default_storage_quota_gb: int = 0
    # --- Background upgrade scan (CollectionManagement Phase 5; opt-in, default OFF).
    # Runs only while upgrade_allowed is ALSO on; enqueues at most
    # background_upgrade_max_per_run origin='upgrade' grabs per sweep.
    background_upgrade_scan_enabled: bool = False
    background_upgrade_scan_interval_hours: int = 12
    background_upgrade_max_per_run: int = 3

    def __post_init__(self) -> None:
        _validate_range(self.download_stall_timeout_minutes, "download_stall_timeout_minutes", 2, 240)
        _validate_range(self.download_queued_timeout_minutes, "download_queued_timeout_minutes", 5, 1440)
        _validate_range(self.max_failover_attempts, "max_failover_attempts", 1, 10)
        _validate_range(self.max_concurrent_downloads, "max_concurrent_downloads", 1, 10)
        _validate_range(self.auto_retry_max_attempts, "auto_retry_max_attempts", 0, 20)
        _validate_range(self.auto_retry_base_interval_minutes, "auto_retry_base_interval_minutes", 1, 1440)
        _validate_range(self.usenet_min_release_age_minutes, "usenet_min_release_age_minutes", 0, 1440)
        _validate_range(self.max_size_mb, "max_size_mb", 0, 1_000_000)
        _validate_range(self.usenet_retention_days, "usenet_retention_days", 0, 100_000)
        _validate_range(self.recycle_retention_days, "recycle_retention_days", 1, 3650)
        _validate_range(self.max_library_size_gb, "max_library_size_gb", 0, 1_000_000)
        _validate_range(self.default_request_quota_count, "default_request_quota_count", 0, 100_000)
        _validate_range(self.default_request_quota_days, "default_request_quota_days", 1, 3650)
        _validate_range(self.default_storage_quota_gb, "default_storage_quota_gb", 0, 1_000_000)
        _validate_range(
            self.background_upgrade_scan_interval_hours,
            "background_upgrade_scan_interval_hours", 1, 720,
        )
        _validate_range(self.background_upgrade_max_per_run, "background_upgrade_max_per_run", 1, 100)
        _rank = {k: r for r, k in enumerate(("low", "mp3_192", "mp3_256", "mp3_320", "lossless"))}
        if self.quality_min not in _rank:
            self.quality_min = "mp3_320"
        if self.quality_max not in _rank:
            self.quality_max = "lossless"
        if _rank[self.quality_min] > _rank[self.quality_max]:
            self.quality_min = self.quality_max
        # Cutoff sits within [min, max]: don't upgrade past what the user accepts, nor stop
        # below the floor. Clamp rather than reject so a stale config can't brick the policy.
        if self.quality_cutoff not in _rank:
            self.quality_cutoff = self.quality_max
        if _rank[self.quality_cutoff] < _rank[self.quality_min]:
            self.quality_cutoff = self.quality_min
        if _rank[self.quality_cutoff] > _rank[self.quality_max]:
            self.quality_cutoff = self.quality_max


class SabnzbdConnectionSettings(AppStruct):
    """SABnzbd download-client connection (D5). ``api_key`` is the FULL key (the add-only
    nzbkey can't do queue/history/delete); encrypted at rest, masked on read. ``category``
    defaults to ``*`` (a fresh SABnzbd has no ``droppedneedle`` category). ``downloads_mount``
    is where DroppedNeedle sees SABnzbd's completed dir (the remap target)."""

    enabled: bool = False
    client_type: str = "sabnzbd"
    url: str = ""
    api_key: str = ""
    category: str = "*"
    priority: int = 0
    post_processing: int = 3
    downloads_mount: str = "/sabnzbd-downloads"

    def __post_init__(self) -> None:
        self.url = self.url.strip()
        if self.url and not self.url.startswith(("http://", "https://")):
            self.url = f"https://{self.url}"
        self.url = self.url.rstrip("/")


class NewznabIndexerSettings(AppStruct):
    """One configured Newznab indexer (D6). ``api_key`` is a Fernet-encrypted
    secret, masked on read and preserved on a masked save - **per array element**.
    DroppedNeedle ships no indexers; the user adds their own (guardrail 1)."""

    id: str = ""
    type: str = "newznab"
    name: str = ""
    url: str = ""
    api_key: str = ""
    categories: list[int] = msgspec.field(default_factory=lambda: [3000, 3010, 3040])
    enabled: bool = True
    priority: int = 1

    def __post_init__(self) -> None:
        self.url = self.url.strip()
        if self.url and not self.url.startswith(("http://", "https://")):
            self.url = f"https://{self.url}"
        self.url = self.url.rstrip("/")


class JellyfinConnectionSettings(AppStruct):
    jellyfin_url: str = "http://jellyfin:8096"
    api_key: str = ""
    user_id: str = ""
    enabled: bool = False
    login_enabled: bool = False

    def __post_init__(self) -> None:
        self.jellyfin_url = self.jellyfin_url.rstrip("/")


class OIDCConnectionSettings(AppStruct):
    enabled: bool = False
    issuer: str = ""
    client_id: str = ""
    client_secret: str = ""
    scopes: str = "openid email profile"
    redirect_uri: str = ""


OIDC_SECRET_MASK = "oidc****"
NAVIDROME_PASSWORD_MASK = "********"
PLEX_TOKEN_MASK = "plex****"
ACOUSTID_KEY_MASK = "acoustid****"
DOWNLOAD_CLIENT_API_KEY_MASK = "slskd****"
INDEXER_API_KEY_MASK = "indexer****"
SABNZBD_API_KEY_MASK = "sabnzbd****"
SPOTIFY_SECRET_MASK = "spotify****"


class SpotifySettings(AppStruct):
    client_id: str = ""
    client_secret: str = ""
    enabled: bool = False

# keep in sync with NamingTemplateEngine.DEFAULT (services/native/naming.py)
DEFAULT_NAMING_TEMPLATE = "{albumartist}/{album} ({year})/{disc:02d}{track:02d} {title}.{ext}"


class LibrarySettings(AppStruct):
    """Native library config (lives in PreferencesService, not config.py).

    acoustid_api_key is a Fernet-encrypted secret, masked on read, preserved on
    save when the masked sentinel comes back unchanged.
    """

    library_paths: list[str] = msgspec.field(default_factory=lambda: ["/music"])
    staging_path: str = ""
    naming_template: str = DEFAULT_NAMING_TEMPLATE
    acoustid_api_key: str = ""


class LibraryPathRequest(AppStruct):
    path: str


class NavidromeConnectionSettings(AppStruct):
    navidrome_url: str = ""
    username: str = ""
    password: str = ""
    enabled: bool = False

    def __post_init__(self) -> None:
        self.navidrome_url = self.navidrome_url.rstrip("/") if self.navidrome_url else ""


class PlexConnectionSettings(AppStruct):
    plex_url: str = ""
    plex_token: str = ""
    enabled: bool = False
    login_enabled: bool = False
    music_library_ids: list[str] = []
    scrobble_to_plex: bool = True

    def __post_init__(self) -> None:
        self.plex_url = self.plex_url.rstrip("/") if self.plex_url else ""


class PlexVerifyResponse(AppStruct):
    valid: bool
    message: str
    libraries: list[PlexLibrarySectionInfo] = []


class PlexOAuthPinResponse(AppStruct):
    pin_id: int
    pin_code: str
    auth_url: str


class PlexOAuthPollResponse(AppStruct):
    completed: bool
    auth_token: str = ""


class JellyfinUserInfo(AppStruct):
    id: str
    name: str


class JellyfinVerifyResponse(AppStruct):
    success: bool
    message: str
    users: list[JellyfinUserInfo] = []


class ListenBrainzConnectionSettings(AppStruct):
    username: str = ""
    user_token: str = ""
    enabled: bool = False


class YouTubeConnectionSettings(AppStruct):
    api_key: str = ""
    enabled: bool = False
    api_enabled: bool = False
    daily_quota_limit: int = 80

    def __post_init__(self) -> None:
        if self.daily_quota_limit < 1 or self.daily_quota_limit > 10000:
            raise msgspec.ValidationError("daily_quota_limit must be between 1 and 10000")

    def has_valid_api_key(self) -> bool:
        return bool(self.api_key and self.api_key.strip())


# Vestigial: kept only so old config.json files with a `home_settings` section
# still parse. Section visibility is now per-user (user_section_prefs).
class HomeSettings(AppStruct):
    cache_ttl_trending: int = 3600
    cache_ttl_personal: int = 300
    show_whats_hot: bool = True
    show_globally_trending: bool = True

    def __post_init__(self) -> None:
        if self.cache_ttl_trending < 300 or self.cache_ttl_trending > 86400:
            raise msgspec.ValidationError("cache_ttl_trending must be between 300 and 86400")
        if self.cache_ttl_personal < 60 or self.cache_ttl_personal > 3600:
            raise msgspec.ValidationError("cache_ttl_personal must be between 60 and 3600")


class LocalFilesConnectionSettings(AppStruct):
    enabled: bool = False
    music_path: str = "/music"
    library_root_path: str = "/music"


class LibrarySyncSettings(AppStruct):
    sync_frequency: Literal["manual", "5min", "10min", "30min", "1hr", "6hr", "12hr", "24hr", "3d", "7d"] = "24hr"
    last_sync: int | None = None
    last_sync_success: bool = True


class LibraryScanScheduleSettings(AppStruct):
    """Native automatic-scan schedule. The saved sync_frequency is migrated once
    on first read. When scan_frequency is "daily" the scan runs once a day at
    daily_scan_time (server-local HH:MM); the interval values run on a rolling gap
    since the last scan."""

    scan_frequency: Literal["manual", "5min", "10min", "30min", "1hr", "6hr", "12hr", "24hr", "3d", "7d", "daily"] = "24hr"
    daily_scan_time: Annotated[str, msgspec.Meta(pattern=r"^([01]\d|2[0-3]):[0-5]\d$")] = "03:00"
    last_scan: int | None = None
    last_scan_success: bool = True


class LibraryScanScheduleResponse(LibraryScanScheduleSettings):
    """GET payload: the persisted schedule plus the server's timezone label, so the
    UI can show what "daily at HH:MM" is relative to. server_timezone is computed per
    request and never persisted."""

    server_timezone: str = ""


class ScrobbleSettings(AppStruct):
    scrobble_to_lastfm: bool = False
    scrobble_to_listenbrainz: bool = False


class PrimaryMusicSourceSettings(AppStruct):
    source: Literal["listenbrainz", "lastfm"] = "listenbrainz"


_OFFICIAL_MB_RATE_LIMIT = 1.0
_OFFICIAL_MB_CONCURRENT_SEARCHES = 6


def is_official_musicbrainz(url: str) -> bool:
    from urllib.parse import urlparse
    try:
        parsed = urlparse(url.strip().rstrip("/"))
        hostname = (parsed.hostname or "").lower()
        return hostname in ("musicbrainz.org", "www.musicbrainz.org")
    except (ValueError, AttributeError):
        return False


class SecuritySettings(AppStruct):
    hibp_check: bool = True
    # local HIBP "Pwned Passwords" file; when present, used instead of the API
    hibp_local_path: str = ""

    # HSTS: only enable when serving over HTTPS
    hsts_max_age: int = 0            # seconds; 0 = disabled
    hsts_include_subdomains: bool = False
    hsts_preload: bool = False


class MusicBrainzConnectionSettings(AppStruct):
    api_url: str = "https://musicbrainz.org/ws/2"
    rate_limit: float = 1.0
    concurrent_searches: int = 6

    def __post_init__(self) -> None:
        self.api_url = self.api_url.strip()
        if not self.api_url or not self.api_url.startswith(("http://", "https://")):
            self.api_url = "https://musicbrainz.org/ws/2"
        self.api_url = self.api_url.rstrip("/")
        if is_official_musicbrainz(self.api_url):
            self.rate_limit = min(self.rate_limit, _OFFICIAL_MB_RATE_LIMIT)
            self.concurrent_searches = min(self.concurrent_searches, _OFFICIAL_MB_CONCURRENT_SEARCHES)
        if self.rate_limit < 0.1 or self.rate_limit > 50.0:
            raise msgspec.ValidationError("rate_limit must be between 0.1 and 50.0")
        if self.concurrent_searches < 1 or self.concurrent_searches > 30:
            raise msgspec.ValidationError("concurrent_searches must be between 1 and 30")


class ConnectAppsSettings(AppStruct):
    """Inbound Connect Apps (Server of Servers) config. Non-secret; persisted
    as a plain PreferencesService section. Both protocols default OFF."""

    subsonic_enabled: bool = False
    jellyfin_enabled: bool = False
    transcoding_enabled: bool = True
    transcode_default_format: Literal["mp3", "opus"] = "mp3"
    transcode_max_bitrate_kbps: int = 320
    advertise_server_name: str = "DroppedNeedle"
    advertise_server_version: str = "10.10.6"
    discover_mode: Literal["local-only", "lazy-mb", "use-scrobble-targets"] = "local-only"

    def __post_init__(self) -> None:
        if self.transcode_max_bitrate_kbps < 32 or self.transcode_max_bitrate_kbps > 1411:
            raise msgspec.ValidationError(
                "transcode_max_bitrate_kbps must be between 32 and 1411"
            )


