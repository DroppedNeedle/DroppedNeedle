from typing import Any


class DroppedNeedleException(Exception):
    def __init__(self, message: str, details: Any = None):
        self.message = message
        self.details = details
        super().__init__(message)

    def __str__(self) -> str:
        if self.details:
            return f"{self.message}: {self.details}"
        return self.message


class ExternalServiceError(DroppedNeedleException):
    pass


class NonRetriableExternalServiceError(ExternalServiceError):
    """A provider response is deterministic and must not be retried."""


class RateLimitedError(ExternalServiceError):
    def __init__(
        self,
        message: str,
        details: Any = None,
        retry_after_seconds: float | None = None,
    ):
        super().__init__(message, details)
        self.retry_after_seconds = retry_after_seconds


class ServiceDisabledUpstreamError(DroppedNeedleException):
    """A provider has deliberately switched a sub-API off (e.g. ListenBrainz's
    "Popularity API currently disabled due to high load"). Deliberately NOT an
    ExternalServiceError: it must not be retried (deterministic for the outage)
    and must not trip the provider's shared circuit breaker (the rest of the
    provider's endpoints are healthy)."""


class InvalidExternalPayloadError(ExternalServiceError):
    """A provider answered successfully but its payload violates the verified
    response schema (e.g. a MusicBrainz field that arrives as JSON null but was
    modelled as required). Deterministic per payload: it must not count as a
    service-health failure on the shared circuit breaker (pass through
    ``non_breaking_exceptions`` at the ``with_retry`` call site) - the service is
    healthy, the schema expectation was wrong. Stays an ExternalServiceError so
    callers degrade through their normal paths."""


class ResourceNotFoundError(DroppedNeedleException):
    pass


class ValidationError(DroppedNeedleException):
    pass


class ArtworkProcessingError(ValidationError):
    """Artwork bytes cannot be admitted or transformed safely."""

    pass


class AudioFormatError(ValidationError):
    """An admitted audio path cannot be represented by a verified adapter."""

    pass


class AudioFormatMismatchError(AudioFormatError):
    pass


class UnsupportedAudioFormatError(AudioFormatError):
    pass


class AudioWriteError(AudioFormatError):
    """A staged audio mutation or validation failed before publication."""

    pass


class ScriptValidationError(ValidationError):
    """A bounded management script failed syntax or runtime validation."""

    def __init__(
        self,
        message: str,
        *,
        script_name: str,
        line: int,
        column: int,
    ) -> None:
        self.script_name = script_name
        self.line = line
        self.column = column
        super().__init__(f"{script_name}:{line}:{column}: {message}")


class PathLimitExceededError(ScriptValidationError):
    """A rendered management path exceeded an enabled length limit."""


class ProviderIdentityRequiredError(ValidationError):
    error_code = "PROVIDER_IDENTITY_REQUIRED"


class ExactReleaseMappingIncompleteError(ValidationError):
    error_code = "EXACT_RELEASE_MAPPING_INCOMPLETE"


class CustomEditionNotSealableError(ValidationError):
    error_code = "CUSTOM_EDITION_NOT_SEALABLE"


class RangeNotSatisfiableError(ValidationError):
    def __init__(self, file_size: int):
        super().__init__("Requested byte range is not satisfiable")
        self.file_size = file_size


class PermissionDeniedError(DroppedNeedleException):
    """Ownership/authorization violation. Mapped to HTTP 403 by the registered handler."""

    pass


class ConflictError(DroppedNeedleException):
    """Duplicate active request/download. Mapped to HTTP 409 by the registered handler."""

    pass


class LibraryManagementDestinationConflictError(ConflictError):
    """A staged Library Management destination is occupied or aliases another path."""

    pass


class MediaAccountRelinkRequiredError(ConflictError):
    """A linked media-server account exists but cannot be used safely."""

    error_code = "MEDIA_ACCOUNT_RELINK_REQUIRED"


class PlaylistNotFoundError(ResourceNotFoundError):
    pass


class InvalidPlaylistDataError(ValidationError):
    pass


class SourceResolutionError(ValidationError):
    pass


class ConfigurationError(DroppedNeedleException):
    pass


class StaleRevisionError(ConflictError):
    pass


class LibraryManagementPolicyChangedError(StaleRevisionError):
    """The Library Management policy or root projection changed during publication."""

    pass


class AutomaticManagementHoldError(DroppedNeedleException):
    """A verified import unit must remain intact until management can be retried."""

    def __init__(self, reason_code: str, message: str) -> None:
        super().__init__(message)
        self.reason_code = reason_code


class ContributionStateError(ConflictError):
    error_code = "CONTRIBUTION_STATE_CONFLICT"


class ContributionProviderExpiredError(ConflictError):
    error_code = "CONTRIBUTION_PROVIDER_EXPIRED"


class ContributionDuplicateCheckRequiredError(ConflictError):
    error_code = "CONTRIBUTION_DUPLICATE_CHECK_REQUIRED"


class ContributionExactDuplicateError(ConflictError):
    error_code = "CONTRIBUTION_EXACT_DUPLICATE"


class ContributionResultMismatchError(ConflictError):
    error_code = "CONTRIBUTION_RESULT_MISMATCH"


class ContributionDataError(DroppedNeedleException):
    """A persisted contribution document cannot be decoded safely."""

    pass


class DiscogsApiError(ExternalServiceError):
    """Discogs returned an unusable response for optional contribution metadata."""

    pass


class RevisionOverflowError(DroppedNeedleException):
    pass


class TargetStartupInvariantError(DroppedNeedleException):
    pass


class PlaybackNotAllowedError(ExternalServiceError):
    pass


class TokenNotAuthorizedError(ExternalServiceError):
    pass


class PlexApiError(ExternalServiceError):
    def __init__(
        self,
        message: str,
        details: Any = None,
        code: int | None = None,
    ):
        super().__init__(message, details)
        self.code = code


class PlexAuthError(PlexApiError):
    pass


class JellyfinAuthError(ExternalServiceError):
    """Outbound Jellyfin rejected our credential (401).

    Non-breaking for the shared circuit breaker: with per-user access tokens a
    single revoked/stale token must not open the circuit for every user.
    """

    pass


class NavidromeApiError(ExternalServiceError):
    def __init__(
        self,
        message: str,
        details: Any = None,
        code: int | None = None,
    ):
        super().__init__(message, details)
        self.code = code


class NavidromeAuthError(NavidromeApiError):
    pass


class NavidromeSubsonicError(ExternalServiceError):
    """Non-auth error from a valid Subsonic API envelope.

    Raised when Navidrome returns a well-formed ``subsonic-response`` with
    a non-OK status and an error code that is *not* an authentication
    failure (codes 40/41).  These are potentially transient (e.g. "Library
    not found or empty" during a rescan) and should be retried but must
    **not** trip the circuit breaker.
    """

    def __init__(
        self,
        message: str,
        details: Any = None,
        code: int | None = None,
    ):
        super().__init__(message, details)
        self.code = code


class SlskdApiError(ExternalServiceError):
    """Transport/HTTP error talking to slskd (AUD-10).

    Mapped to HTTP 503 by the registered ``ExternalServiceError`` handler -
    no separate handler registration is needed. Mirrors the
    ``PlexApiError``/``NavidromeApiError`` precedent.
    """

    def __init__(
        self,
        message: str,
        details: Any = None,
        code: int | None = None,
    ):
        super().__init__(message, details)
        self.code = code


class NewznabApiError(ExternalServiceError):
    """Transport/HTTP/feed error talking to a Newznab indexer.

    Mapped to HTTP 503 by the registered ``ExternalServiceError`` handler.
    ``code`` is the Newznab ``<error code>`` when the indexer returned one (HTTP
    may still be 200), else the HTTP status. Mirrors ``SlskdApiError``.
    """

    def __init__(
        self,
        message: str,
        details: Any = None,
        code: int | None = None,
    ):
        super().__init__(message, details)
        self.code = code


class NewznabAuthError(NewznabApiError):
    """Newznab auth failure (error code 100-199, or a missing/invalid API key)."""

    pass


class LidarrImportError(ExternalServiceError):
    """Transport/HTTP/decode error talking to the read-only Lidarr importer's Lidarr
    instance (LidarrImport). Mapped to HTTP 503 ``EXTERNAL_SERVICE_UNAVAILABLE`` by the
    registered ``ExternalServiceError`` handler - no separate handler needed. The finer
    reachable/bad-key/version distinctions are carried in the ``LidarrTestResponse`` body
    (``auth`` flags a rejected API key), never a leaked exception body (5xx bodies stay
    generic)."""

    def __init__(
        self, message: str, details: Any = None, *, auth: bool = False
    ) -> None:
        super().__init__(message, details)
        self.auth = auth


class TicketmasterApiError(ExternalServiceError):
    """Transport/HTTP/decode error talking to the Ticketmaster Discovery API.

    Mapped to HTTP 503 by the registered ``ExternalServiceError`` handler.
    """

    pass


class SkiddleApiError(ExternalServiceError):
    """Transport/HTTP/decode/``error!=0`` failure talking to the Skiddle API.

    Mapped to HTTP 503 by the registered ``ExternalServiceError`` handler.
    """

    pass


class LrclibApiError(ExternalServiceError):
    """Transport, HTTP, or decode failure from the LRCLIB lyrics service."""

    pass


class GeocodingApiError(ExternalServiceError):
    """Transport/HTTP/decode error talking to the Open-Meteo geocoding API
    (the events city picker). Mapped to HTTP 503 - a failed city search must
    surface as 'geocoding unavailable', never as an empty result list.
    """

    pass


class ClientDisconnectedError(DroppedNeedleException):
    pass


class AuthenticationError(DroppedNeedleException):
    pass


class RegistrationError(DroppedNeedleException):
    pass


class SubsonicError(DroppedNeedleException):
    """Inbound OpenSubsonic failure -> rendered as a status=failed envelope.

    Distinct from the outbound ``NavidromeSubsonicError`` (us-as-client): this is
    us-as-server failing a compat client's request. Raised by the Subsonic shim
    and ``AppPasswordService.verify_subsonic``; the per-shim error boundary reads
    ``err.code``. ``code`` is the first positional so ``SubsonicError(43)`` works.
    """

    def __init__(self, code: int, message: str | None = None):
        super().__init__(message or "", None)
        self.code = code


class JellyfinError(DroppedNeedleException):
    """Inbound Jellyfin failure -> rendered as a real HTTP status code.

    Distinct from the outbound Jellyfin client errors: this is us-as-server
    failing a compat client's request. Raised by the Jellyfin shim and its auth
    resolution; the per-shim error boundary reads ``err.status`` / ``err.body``.
    ``status`` is the first positional so ``JellyfinError(401)`` works.
    """

    def __init__(
        self, status: int, message: str | None = None, body: dict | None = None
    ):
        super().__init__(message or "", None)
        self.status = status
        self.body = body
