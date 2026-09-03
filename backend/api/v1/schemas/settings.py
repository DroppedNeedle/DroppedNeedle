import math
import re
from typing import Annotated, Any, Literal, Mapping

import msgspec

from api.v1.schemas.advanced_settings import _validate_range
from api.v1.schemas.plex import PlexLibrarySectionInfo
from infrastructure.http.brainzmash_transport import BRAINZMASH_ENDPOINT
from infrastructure.msgspec_fastapi import AppStruct
from models.acquisition_quality import QualityRecipeEntry, validate_quality_recipe

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
    def from_settings(
        cls, settings: LastFmConnectionSettings
    ) -> "LastFmConnectionSettingsResponse":
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
    primary_types: list[str] = msgspec.field(
        default_factory=lambda: ["album", "ep", "single"]
    )
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
    # Optional ABSOLUTE container path of slskd's incomplete-downloads dir, for the
    # partial-bytes fallback (#292). Absolute only: a relative value cannot be resolved
    # against the complete mount safely, so it is dropped (sanitised below). Empty
    # disables the fallback entirely (today's behaviour). A plain path, not a secret.
    slskd_incomplete_mount: str = ""
    preflight_score_auto_accept: float = 0.70
    preflight_score_manual_min: float = 0.50
    # Download resilience (minutes-scale; user-tunable). A transfer actively
    # connecting/moving bytes that then freezes is a stall; a transfer still
    # sitting in the peer's remote upload queue gets the more generous timeout.
    download_stall_timeout_minutes: int = 30
    download_queued_timeout_minutes: int = 120
    preferred_quality_wait_minutes: int = 15
    max_failover_attempts: int = 3
    max_concurrent_downloads: int = 3
    auto_retry_enabled: bool = True
    auto_retry_max_attempts: int = 6
    auto_retry_base_interval_minutes: int = 15

    def __post_init__(self) -> None:
        # Strip paste whitespace from the key (the "slskd****" mask is
        # strip-identity, so sentinel comparison is unaffected).
        self.api_key = self.api_key.strip() if self.api_key else ""
        # normalise a bare host (e.g. "slskd:5030") to a full URL; httpx rejects a
        # schemeless URL
        self.url = self.url.strip()
        if self.url and not self.url.startswith(("http://", "https://")):
            self.url = f"https://{self.url}"
        self.url = self.url.rstrip("/")
        _validate_range(
            self.download_stall_timeout_minutes,
            "download_stall_timeout_minutes",
            2,
            240,
        )
        _validate_range(
            self.download_queued_timeout_minutes,
            "download_queued_timeout_minutes",
            5,
            1440,
        )
        _validate_range(
            self.preferred_quality_wait_minutes,
            "preferred_quality_wait_minutes",
            1,
            1440,
        )
        _validate_range(self.max_failover_attempts, "max_failover_attempts", 1, 10)
        _validate_range(
            self.max_concurrent_downloads, "max_concurrent_downloads", 1, 10
        )
        _validate_range(self.auto_retry_max_attempts, "auto_retry_max_attempts", 0, 20)
        _validate_range(
            self.auto_retry_base_interval_minutes,
            "auto_retry_base_interval_minutes",
            1,
            1440,
        )
        # mirrors services.native.quality_tiers.TIER_KEYS (best -> worst); keep in sync
        _rank = {
            k: r
            for r, k in enumerate(("low", "mp3_192", "mp3_256", "mp3_320", "lossless"))
        }
        if self.quality_min not in _rank:
            self.quality_min = "mp3_320"
        if self.quality_max not in _rank:
            self.quality_max = "lossless"
        if _rank[self.quality_min] > _rank[self.quality_max]:
            self.quality_min = self.quality_max
        # keep only safe, relative path components - the subpath joins onto the mount, so
        # drop "", ".", ".." and any leading slashes so it can never escape it
        self.downloads_subpath = "/".join(
            p
            for p in re.split(r"[\\/]", self.downloads_subpath.strip())
            if p and p not in (".", "..")
        )
        # same component hygiene for the incomplete mount, but absoluteness is kept:
        # drop "", ".", ".." so ".." can never escape, then re-anchor at root. A
        # relative input (or bare "/") has no safe meaning here, so it becomes "".
        raw_incomplete = self.slskd_incomplete_mount.strip()
        incomplete_parts = [
            p
            for p in re.split(r"[\\/]", raw_incomplete)
            if p and p not in (".", "..")
        ]
        self.slskd_incomplete_mount = (
            "/" + "/".join(incomplete_parts)
            if raw_incomplete.startswith("/") and incomplete_parts
            else ""
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
    # A higher-ranked Soulseek quality/resolution pool gets this shared zero-byte
    # queue window before failover may step down within the accepted quality range.
    preferred_quality_wait_minutes: int = 15
    max_failover_attempts: int = 3
    max_concurrent_downloads: int = 3
    auto_retry_enabled: bool = True
    auto_retry_max_attempts: int = 6
    auto_retry_base_interval_minutes: int = 15
    # Don't permanently blocklist a Usenet release younger than this; let it retry once it
    # has propagated across Usenet servers (review M4 / owner Q2).
    usenet_min_release_age_minutes: int = 30
    # Acquisition-pipeline substrate (ArrRebuild). Source-agnostic; consumed by the
    # decision specs. All default to "off" so behaviour is unchanged until opted in.
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
    # Upgrade/cutoff substrate (full feature lives in CollectionManagement).
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
    # Cost control (CollectionManagement Feature C). 0 = unlimited. Caps BLOCK
    # new non-upgrade grabs at/over the ceiling; nothing is ever auto-evicted (A3).
    # Per-user values are defaults; user_quotas rows override them (NULL = inherit).
    max_library_size_gb: int = 0
    default_request_quota_count: int = 0
    default_request_quota_days: int = 7
    default_storage_quota_gb: int = 0
    # Background upgrade scan (CollectionManagement Phase 5; opt-in, default OFF).
    # Runs only while upgrade_allowed is ALSO on; enqueues at most
    # background_upgrade_max_per_run origin='upgrade' grabs per sweep.
    background_upgrade_scan_enabled: bool = False
    background_upgrade_scan_interval_hours: int = 12
    background_upgrade_max_per_run: int = 3
    # v2 ordered format-quality recipe. Empty preserves the v1 policy shape and
    # semantics; non-empty values are validated strictly before PUT conversion.
    quality_recipe: list[QualityRecipeEntry] = msgspec.field(default_factory=list)
    # Read-only status metadata used when a legacy policy cannot be projected
    # without silently dropping codecs. These values are stripped before save.
    quality_recipe_status: Literal["v1", "v2", "non_convertible", "invalid"] = "v1"
    quality_recipe_error: str | None = None
    # Legacy v1 order/quality fields remain for rollback and old persisted blobs.
    quality_preference_order: list[str] = msgspec.field(default_factory=list)
    preferred_lossy_bitrate_kbps: int | None = None
    lossy_min_bitrate_kbps: int | None = None
    lossy_max_bitrate_kbps: int | None = None
    # Within-lossless detail target/caps - canonical tier stays ``lossless``.
    lossless_preference: str = "highest"  # cd|24_48|24_96|24_192|highest
    lossless_max_bit_depth: int | None = None
    lossless_max_sample_rate_hz: int | None = None
    unknown_quality_behavior: str = (
        "allow_as_fallback"  # reject|review|allow_as_fallback
    )
    source_selection_mode: str = "source_first"  # source_first|quality_first

    def __post_init__(self) -> None:
        _validate_range(
            self.download_stall_timeout_minutes,
            "download_stall_timeout_minutes",
            2,
            240,
        )
        _validate_range(
            self.download_queued_timeout_minutes,
            "download_queued_timeout_minutes",
            5,
            1440,
        )
        _validate_range(
            self.preferred_quality_wait_minutes,
            "preferred_quality_wait_minutes",
            1,
            1440,
        )
        _validate_range(self.max_failover_attempts, "max_failover_attempts", 1, 10)
        _validate_range(
            self.max_concurrent_downloads, "max_concurrent_downloads", 1, 10
        )
        _validate_range(self.auto_retry_max_attempts, "auto_retry_max_attempts", 0, 20)
        _validate_range(
            self.auto_retry_base_interval_minutes,
            "auto_retry_base_interval_minutes",
            1,
            1440,
        )
        _validate_range(
            self.usenet_min_release_age_minutes,
            "usenet_min_release_age_minutes",
            0,
            1440,
        )
        _validate_range(self.max_size_mb, "max_size_mb", 0, 1_000_000)
        _validate_range(self.usenet_retention_days, "usenet_retention_days", 0, 100_000)
        _validate_range(self.recycle_retention_days, "recycle_retention_days", 1, 3650)
        _validate_range(self.max_library_size_gb, "max_library_size_gb", 0, 1_000_000)
        _validate_range(
            self.default_request_quota_count, "default_request_quota_count", 0, 100_000
        )
        _validate_range(
            self.default_request_quota_days, "default_request_quota_days", 1, 3650
        )
        _validate_range(
            self.default_storage_quota_gb, "default_storage_quota_gb", 0, 1_000_000
        )
        _validate_range(
            self.background_upgrade_scan_interval_hours,
            "background_upgrade_scan_interval_hours",
            1,
            720,
        )
        _validate_range(
            self.background_upgrade_max_per_run,
            "background_upgrade_max_per_run",
            1,
            100,
        )
        _rank = {
            k: r
            for r, k in enumerate(("low", "mp3_192", "mp3_256", "mp3_320", "lossless"))
        }
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

        # --- Acquisition-quality field normalization (stored-data resilient,
        # mirrors the legacy tier-key handling above: an invalid STORED value
        # heals to the existing-install default instead of bricking every
        # config load). User-submitted policies go through
        # ``validate_new_quality_fields`` first, which raises instead of
        # healing - submitted fields are never silently clamped.
        from services.native.acquisition.quality import derive_default_order

        accepted = derive_default_order(self.quality_min, self.quality_max)
        order = list(self.quality_preference_order)
        order_ok = len(order) == len(accepted) and sorted(order) == sorted(accepted)
        self.quality_preference_order = order if order_ok else list(accepted)
        _lossless_prefs = {"cd", "24_48", "24_96", "24_192", "highest"}
        if self.lossless_preference not in _lossless_prefs:
            self.lossless_preference = "highest"
        _unknown_rules = {"reject", "review", "allow_as_fallback"}
        if self.unknown_quality_behavior not in _unknown_rules:
            self.unknown_quality_behavior = "allow_as_fallback"
        _source_modes = {"source_first", "quality_first"}
        if self.source_selection_mode not in _source_modes:
            self.source_selection_mode = "source_first"
        for name in (
            "preferred_lossy_bitrate_kbps",
            "lossy_min_bitrate_kbps",
            "lossy_max_bitrate_kbps",
        ):
            value = getattr(self, name)
            if value is not None and not (
                _QUALITY_KBPS_MIN <= value <= _QUALITY_KBPS_MAX
            ):
                setattr(self, name, None)
        if (
            self.lossy_min_bitrate_kbps is not None
            and self.lossy_max_bitrate_kbps is not None
            and self.lossy_min_bitrate_kbps > self.lossy_max_bitrate_kbps
        ):
            self.lossy_min_bitrate_kbps = None
            self.lossy_max_bitrate_kbps = None
        for name, low, high in (
            ("lossless_max_bit_depth", 1, 64),
            ("lossless_max_sample_rate_hz", 8000, 768000),
        ):
            value = getattr(self, name)
            if value is not None and not (low <= value <= high):
                setattr(self, name, None)


_QUALITY_KBPS_MIN = 16
_QUALITY_KBPS_MAX = 2048


def validate_new_quality_fields(payload: Mapping[str, Any]) -> None:
    """STRICT validation of a user-submitted download-policy BODY (the raw
    deserialized mapping, BEFORE ``DownloadPolicySettings`` construction -
    its ``__post_init__`` heals stored-data drift and would erase the evidence).
    Duplicates, missing accepted tiers, out-of-range endpoints, invalid
    bounds, impossible target/bound combinations and unknown enum labels
    raise ValueError; the route maps that to a 400."""
    from services.native.acquisition.quality import normalize_order

    quality_min = payload.get("quality_min", "mp3_320")
    quality_max = payload.get("quality_max", "lossless")
    submitted_order = payload.get("quality_preference_order")
    if submitted_order:  # [] / missing = derive (same contract as GET→PUT round-trip)
        normalize_order(list(submitted_order), quality_min, quality_max)
    submitted_recipe = payload.get("quality_recipe")
    if submitted_recipe is not None:
        if not isinstance(submitted_recipe, list):
            raise ValueError("quality_recipe must be a list")
        if submitted_recipe:
            if payload.get("flac_mp3_only", True) is not True:
                raise ValueError(
                    "quality_recipe requires flac_mp3_only=true; "
                    "replace the non-convertible policy first"
                )
            try:
                entries = [
                    msgspec.convert(item, type=QualityRecipeEntry, strict=True)
                    for item in submitted_recipe
                ]
                validate_quality_recipe(entries)
            except (msgspec.ValidationError, TypeError, ValueError) as exc:
                raise ValueError(str(exc)) from exc
    _lossless_prefs = {"cd", "24_48", "24_96", "24_192", "highest"}
    _unknown_rules = {"reject", "review", "allow_as_fallback"}
    _source_modes = {"source_first", "quality_first"}
    lossless_preference = payload.get("lossless_preference", "highest")
    if lossless_preference not in _lossless_prefs:
        raise ValueError(f"invalid lossless_preference: {lossless_preference!r}")
    unknown_quality_behavior = payload.get(
        "unknown_quality_behavior", "allow_as_fallback"
    )
    if unknown_quality_behavior not in _unknown_rules:
        raise ValueError(
            f"invalid unknown_quality_behavior: {unknown_quality_behavior!r}"
        )
    source_selection_mode = payload.get("source_selection_mode", "source_first")
    if source_selection_mode not in _source_modes:
        raise ValueError(f"invalid source_selection_mode: {source_selection_mode!r}")
    numbers = (
        ("preferred_lossy_bitrate_kbps", payload.get("preferred_lossy_bitrate_kbps")),
        ("lossy_min_bitrate_kbps", payload.get("lossy_min_bitrate_kbps")),
        ("lossy_max_bitrate_kbps", payload.get("lossy_max_bitrate_kbps")),
    )
    for name, value in numbers:
        if value is not None and not (_QUALITY_KBPS_MIN <= value <= _QUALITY_KBPS_MAX):
            raise ValueError(
                f"{name} must be between {_QUALITY_KBPS_MIN} and {_QUALITY_KBPS_MAX} kbps"
            )
    lossy_min = payload.get("lossy_min_bitrate_kbps")
    lossy_max = payload.get("lossy_max_bitrate_kbps")
    if lossy_min is not None and lossy_max is not None and lossy_min > lossy_max:
        raise ValueError("lossy_min_bitrate_kbps exceeds lossy_max_bitrate_kbps")
    target = payload.get("preferred_lossy_bitrate_kbps")
    if target is not None:
        if (lossy_min is not None and target < lossy_min) or (
            lossy_max is not None and target > lossy_max
        ):
            raise ValueError(
                "preferred_lossy_bitrate_kbps must sit inside "
                "[lossy_min_bitrate_kbps, lossy_max_bitrate_kbps]"
            )
    depth = payload.get("lossless_max_bit_depth")
    if depth is not None and not 1 <= depth <= 64:
        raise ValueError("lossless_max_bit_depth must be 1..64 bits")
    rate = payload.get("lossless_max_sample_rate_hz")
    if rate is not None and not 8000 <= rate <= 768000:
        raise ValueError("lossless_max_sample_rate_hz must be 8000..768000 Hz")


class WantedWatcherSettings(AppStruct):
    """The wanted watcher (Wanted plan §5.4): granular opt-out toggles, no secrets.
    Cadence stays code constants on purpose - fewer knobs."""

    enabled: bool = True  # master switch (rollback lever, read per sweep)
    auto_download_on_find: bool = True  # D2; off = badge-only even for auto-tier finds
    watch_partial_albums: bool = True  # D6
    max_checks_per_sweep: int = 3
    dormant_after_days: int = 365

    def __post_init__(self) -> None:
        _validate_range(self.max_checks_per_sweep, "max_checks_per_sweep", 1, 20)
        _validate_range(self.dormant_after_days, "dormant_after_days", 30, 3650)


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
    secret, masked on read and preserved on a masked save - per array element.
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


class LidarrImportConnectionSettings(AppStruct):
    """Read-only Lidarr *import* connection (LidarrImport D5): a single admin-configured
    Lidarr the monitored-artist importer reads from. NOT a management integration - the old
    Lidarr surface stays deleted (D8). ``api_key`` is a Fernet-encrypted secret, masked on
    read, preserved on a masked save."""

    url: str = ""
    api_key: str = ""

    def __post_init__(self) -> None:
        # Normalise the base URL to a bare origin we can append /api/v1 to ourselves:
        # default a scheme-less host to http:// (Lidarr is a plain-http LAN service - do NOT
        # copy SABnzbd's https-forcing, see 02-lidarr-api.md), strip a trailing '/', then strip
        # a trailing '/api/v1' or '/api' if the user pasted the full API path. Because this
        # normalises silently, the Test route needs no "did you mean /api" suggestion.
        self.url = self.url.strip()
        if self.url and not self.url.startswith(("http://", "https://")):
            self.url = f"http://{self.url}"
        self.url = self.url.rstrip("/")
        for suffix in ("/api/v1", "/api"):
            if self.url.endswith(suffix):
                self.url = self.url[: -len(suffix)].rstrip("/")
                break


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
LIDARR_IMPORT_API_KEY_MASK = "lidarr****"
SPOTIFY_SECRET_MASK = "spotify****"


class SpotifySettings(AppStruct):
    # GH-298: optional admin-configured origin (scheme://host[:port]) the OAuth
    # redirect URI is built from; empty keeps the request.base_url fallback.
    # Not a secret - returned unmasked like client_id.
    client_id: str = ""
    client_secret: str = ""
    enabled: bool = False
    spotify_redirect_origin: str = ""


TICKETMASTER_KEY_MASK = "ticketmaster****"
SKIDDLE_KEY_MASK = "skiddle****"


class PluginConfig(AppStruct):
    """Per-plugin admin state (phase 01b). ``enabled`` defaults to False on
    purpose: dropping a folder into the plugins directory must never run code
    until an admin flips it on (the documented trust model). ``settings`` holds
    the values for the fields the plugin's manifest declares."""

    enabled: bool = False
    settings: dict[str, str] = {}


class FreeMusicSettings(AppStruct):
    """Free Music (D24): DroppedNeedle's own lawful download client. Downloads
    Creative Commons and public-domain music from the Internet Archive, filtered
    to items carrying an explicit licence. Enabled by default - it costs nothing,
    needs no signup, and the lawful use it demonstrates is what makes having a
    download engine defensible at all."""

    enabled: bool = True
    preferred_format: Literal["flac", "mp3"] = "flac"


class GetItSettings(AppStruct):
    """ "Get it" purchase links (phase 01). ``store_region`` feeds the iTunes
    Search ``country`` storefront parameter."""

    store_region: Annotated[str, msgspec.Meta(pattern=r"^[A-Za-z]{2}$")] = "US"


class EventsSettings(AppStruct):
    """Upcoming Events sources (.dev-notes/Events). Both API keys are
    Fernet-encrypted secrets, masked on read, preserved on save when the
    masked sentinel comes back unchanged. The sweep runs daily at
    ``poll_time`` (server-local HH:MM, the daily_scan_time pattern)."""

    enabled: bool = False
    ticketmaster_enabled: bool = False
    ticketmaster_api_key: str = ""
    skiddle_enabled: bool = False
    skiddle_api_key: str = ""
    poll_time: Annotated[str, msgspec.Meta(pattern=r"^([01]\d|2[0-3]):[0-5]\d$")] = (
        "06:00"
    )
    # 'followed' sweeps the distinct followed artists; 'library' additionally
    # sweeps every artist in the library index (and shows those events to all)
    sweep_scope: Literal["followed", "library"] = "followed"


# keep in sync with NamingTemplateEngine.DEFAULT (services/native/naming.py)
DEFAULT_NAMING_TEMPLATE = (
    "{albumartist}/{album} ({year})/{disc:02d}{track:02d} {title}.{ext}"
)


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
        self.navidrome_url = (
            self.navidrome_url.rstrip("/") if self.navidrome_url else ""
        )


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
            raise msgspec.ValidationError(
                "daily_quota_limit must be between 1 and 10000"
            )

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
            raise msgspec.ValidationError(
                "cache_ttl_trending must be between 300 and 86400"
            )
        if self.cache_ttl_personal < 60 or self.cache_ttl_personal > 3600:
            raise msgspec.ValidationError(
                "cache_ttl_personal must be between 60 and 3600"
            )


class LocalFilesConnectionSettings(AppStruct):
    enabled: bool = False
    music_path: str = "/music"
    library_root_path: str = "/music"


class LibrarySyncSettings(AppStruct):
    sync_frequency: Literal[
        "manual", "5min", "10min", "30min", "1hr", "6hr", "12hr", "24hr", "3d", "7d"
    ] = "24hr"
    last_sync: int | None = None
    last_sync_success: bool = True


class LibraryScanScheduleSettings(AppStruct):
    """Native automatic-scan schedule. The saved sync_frequency is migrated once
    on first read. When scan_frequency is "daily" the scan runs once a day at
    daily_scan_time (server-local HH:MM); the interval values run on a rolling gap
    since the last scan."""

    scan_frequency: Literal[
        "manual",
        "5min",
        "10min",
        "30min",
        "1hr",
        "6hr",
        "12hr",
        "24hr",
        "3d",
        "7d",
        "daily",
    ] = "24hr"
    daily_scan_time: Annotated[
        str, msgspec.Meta(pattern=r"^([01]\d|2[0-3]):[0-5]\d$")
    ] = "03:00"
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
_BRAINZMASH_RATE_LIMIT = 10.0
_BRAINZMASH_CONCURRENT_SEARCHES = 1

# BrainzMash is deliberately a server-owned source.  The endpoint and disclosure
# version are never accepted from a browser request.
BRAINZMASH_DISCLOSURE_VERSION = "brainzmash-v1"
MusicBrainzSourceMode = Literal["official", "mirror", "community", "brainzmash"]


# P2 full-mirror tier (owner decision 2026-08-24): non-official endpoints are
# user-owned or deliberately chosen infrastructure, so throughput there is a
# user decision within these widened bounds. rate_limit == 0 is the "Unlimited"
# sentinel: valid OFF-OFFICIAL ONLY - it bypasses the client-side limiter for
# that host entirely. The official ceilings below are never raised.
_MAX_MB_RATE_LIMIT = 500.0
_MAX_MB_CONCURRENT_SEARCHES = 64


def is_musicbrainz_rate_policy_public_host(url: str) -> bool:
    from repositories.musicbrainz_base import is_mb_rate_policy_public_host

    return is_mb_rate_policy_public_host(url)


class BrainzMashPendingProposal(AppStruct):
    endpoint: str = BRAINZMASH_ENDPOINT
    access_revision: str = ""
    source_id: str = ""
    generation: int = 0
    disclosure_version: str = BRAINZMASH_DISCLOSURE_VERSION
    consented: bool = False
    verified: bool = False


class BrainzMashActiveBinding(AppStruct):
    endpoint: str = BRAINZMASH_ENDPOINT
    access_revision: str = ""
    source_id: str = ""
    generation: int = 0
    disclosure_version: str = BRAINZMASH_DISCLOSURE_VERSION
    consented: bool = False
    verified: bool = False


class MusicBrainzBindingRequest(AppStruct):
    access_revision: str
    source_id: str
    generation: int
    disclosure_version: str


class MusicBrainzSettingsUpdate(AppStruct):
    source_mode: MusicBrainzSourceMode = "official"
    api_url: str | None = None
    rate_limit: float = 1.0
    concurrent_searches: int = 6
    community_acknowledged: bool | None = False

    def __post_init__(self) -> None:
        if self.source_mode in {"mirror", "community"} and not self.api_url:
            raise msgspec.ValidationError(
                "api_url is required for mirror and community sources"
            )
        if self.source_mode == "official":
            from repositories.musicbrainz_base import OFFICIAL_MB_API_BASE

            self.api_url = OFFICIAL_MB_API_BASE
        elif self.api_url is not None:
            self.api_url = self.api_url.strip().rstrip("/")
            if self.source_mode in {
                "mirror",
                "community",
            } and not self.api_url.startswith(("http://", "https://")):
                raise msgspec.ValidationError(
                    "api_url must be an absolute HTTP(S) URL for non-official sources"
                )
        if not math.isfinite(self.rate_limit):
            raise msgspec.ValidationError("rate_limit must be finite")
        if self.concurrent_searches < 1:
            raise msgspec.ValidationError("concurrent_searches must be at least 1")


class SecuritySettings(AppStruct):
    hibp_check: bool = True
    # local HIBP "Pwned Passwords" file; when present, used instead of the API
    hibp_local_path: str = ""

    # HSTS: only enable when serving over HTTPS
    hsts_max_age: int = 0  # seconds; 0 = disabled
    hsts_include_subdomains: bool = False
    hsts_preload: bool = False


class MusicBrainzConnectionSettings(AppStruct):
    source_mode: MusicBrainzSourceMode = "official"
    api_url: str = "https://musicbrainz.org/ws/2"
    rate_limit: float = 1.0
    concurrent_searches: int = 6
    community_acknowledged: bool = False
    selected_source_mode: MusicBrainzSourceMode = "official"
    source_id: str = ""
    generation: int = 0
    pending_brainzmash: BrainzMashPendingProposal | None = None
    active_brainzmash: BrainzMashActiveBinding | None = None
    source_quarantined: bool = False
    quarantine_reason: str = ""
    # Surfaced on the save/settings response: True when the official-host clamp
    # had to force entered values down (or lift a sentinel 0 up) to the official
    # limits. The frontend renders this as "values were clamped to official
    # limits" - never a refusal, always applied.
    clamped_to_official_limits: bool = False

    def __post_init__(self) -> None:
        from repositories.musicbrainz_base import OFFICIAL_MB_API_BASE

        if self.source_mode not in {
            "official",
            "mirror",
            "community",
            "brainzmash",
        }:
            raise msgspec.ValidationError("invalid MusicBrainz source_mode")
        if self.selected_source_mode not in {
            "official",
            "mirror",
            "community",
            "brainzmash",
        }:
            raise msgspec.ValidationError("invalid selected MusicBrainz source_mode")
        self.api_url = self.api_url.strip()
        if self.source_mode == "official":
            self.api_url = OFFICIAL_MB_API_BASE
        elif self.source_mode == "brainzmash":
            self.api_url = BRAINZMASH_ENDPOINT.rstrip("/")
        elif not self.api_url or not self.api_url.startswith(("http://", "https://")):
            raise msgspec.ValidationError(
                "api_url must be an absolute HTTP(S) URL for non-official sources"
            )
        self.api_url = self.api_url.rstrip("/")
        self.clamped_to_official_limits = False
        if not math.isfinite(self.rate_limit):
            raise msgspec.ValidationError("rate_limit must be finite")
        if self.concurrent_searches < 1:
            raise msgspec.ValidationError("concurrent_searches must be at least 1")
        if self.source_mode == "brainzmash":
            self.rate_limit = _BRAINZMASH_RATE_LIMIT
            self.concurrent_searches = _BRAINZMASH_CONCURRENT_SEARCHES
        if is_musicbrainz_rate_policy_public_host(self.api_url):
            before = (self.rate_limit, self.concurrent_searches)
            self.rate_limit = min(self.rate_limit, _OFFICIAL_MB_RATE_LIMIT)
            self.concurrent_searches = min(
                self.concurrent_searches, _OFFICIAL_MB_CONCURRENT_SEARCHES
            )
            # The Unlimited sentinel is off-official only: on the official host
            # a 0 lifts to the official rate instead of disabling the limiter.
            if self.rate_limit <= 0:
                self.rate_limit = _OFFICIAL_MB_RATE_LIMIT
            self.clamped_to_official_limits = before != (
                self.rate_limit,
                self.concurrent_searches,
            )
            return

        if (
            self.rate_limit < 0
            or (0 < self.rate_limit < 0.1)
            or (self.rate_limit > _MAX_MB_RATE_LIMIT)
        ):
            raise msgspec.ValidationError(
                "rate_limit must be 0 (unlimited) or between 0.1 and "
                f"{_MAX_MB_RATE_LIMIT}"
            )
        if (
            self.concurrent_searches < 1
            or self.concurrent_searches > _MAX_MB_CONCURRENT_SEARCHES
        ):
            raise msgspec.ValidationError(
                f"concurrent_searches must be between 1 and {_MAX_MB_CONCURRENT_SEARCHES}"
            )


def is_brainzmash_active_binding_valid(
    settings: MusicBrainzConnectionSettings,
) -> bool:
    """Return whether the pinned built-in BrainzMash source may serve traffic.

    BrainzMash is the built-in source, so an interactive disclosure/verification
    binding is optional rather than a prerequisite for the default. The source
    identity and canonical endpoint checks still fence caches and transport to
    the approved origin.
    """

    def exact_nonblank(value: str) -> bool:
        return bool(value) and value == value.strip()

    return bool(
        not settings.source_quarantined
        and settings.source_mode == "brainzmash"
        and settings.api_url == BRAINZMASH_ENDPOINT.rstrip("/")
        and settings.generation > 0
        and exact_nonblank(settings.source_id)
    )


class ConnectAppsSettings(AppStruct):
    """Inbound Connect Apps (Server of Servers) config. Non-secret; persisted
    as a plain PreferencesService section. Both protocols default OFF."""

    subsonic_enabled: bool = False
    jellyfin_enabled: bool = False
    # Capability negotiation for clients that must distinguish the historical
    # exact-track endpoint (which bypassed approval) from the approval-safe
    # implementation. Older servers omit this field, so clients fail closed.
    exact_track_approval_supported: bool = True
    transcoding_enabled: bool = True
    transcode_default_format: Literal["mp3", "opus"] = "mp3"
    transcode_max_bitrate_kbps: int = 320
    advertise_server_name: str = "DroppedNeedle"
    advertise_server_version: str = "10.10.6"
    discover_mode: Literal["local-only", "lazy-mb", "use-scrobble-targets"] = (
        "local-only"
    )

    def __post_init__(self) -> None:
        if (
            self.transcode_max_bitrate_kbps < 32
            or self.transcode_max_bitrate_kbps > 1411
        ):
            raise msgspec.ValidationError(
                "transcode_max_bitrate_kbps must be between 32 and 1411"
            )


WRAPPED_API_KEY_MASK = "••••••••"


class WrappedSettings(AppStruct):
    """Shared secret for the /api/v1/wrapped/* endpoints (service-to-service,
    e.g. a newsletterr integration pulling year-in-review stats). Secret;
    persisted encrypted via PreferencesService like the Last.fm/Spotify
    connection settings."""

    api_key: str = ""


class WrappedSettingsResponse(AppStruct):
    api_key: str = ""

    @classmethod
    def from_settings(cls, settings: WrappedSettings) -> "WrappedSettingsResponse":
        return cls(api_key=_mask_secret(settings.api_key))
