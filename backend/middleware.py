import logging
import time
from typing import Annotated

from fastapi import Depends, HTTPException, Request, status
from starlette.middleware.base import BaseHTTPMiddleware
from starlette.types import ASGIApp
from core.base_path import application_path

from infrastructure.degradation import (
    init_degradation_context,
    try_get_degradation_context,
    clear_degradation_context,
)
from infrastructure.persistence.auth_store import TokenRecord, UserRecord
from infrastructure.resilience.rate_limiter import TokenBucketRateLimiter
from infrastructure.msgspec_fastapi import MsgSpecJSONResponse

logger = logging.getLogger(__name__)

SLOW_REQUEST_THRESHOLD = 1.0

# Exact paths that require no authentication.
# Keep this list narrow, the old "/api/v1/auth/" prefix exempted admin routes too.
_PUBLIC_PATHS: frozenset[str] = frozenset({
    "/health",
    # Auth bootstrap
    "/api/v1/auth/setup/status",
    "/api/v1/auth/setup",
    "/api/v1/auth/providers",
    "/api/v1/auth/login",
    "/api/v1/auth/password-recovery/reset",
    # Logout is public so an expired session can still clear the cookie
    "/api/v1/auth/logout",
    # Third-party login flows
    "/api/v1/auth/plex/pin",
    "/api/v1/auth/plex/poll",
    "/api/v1/auth/jellyfin/login",
    "/api/v1/auth/oidc/authorize",
    "/api/v1/auth/oidc/callback",
    "/api/v1/auth/oidc/exchange",
    # Spotify OAuth callback identifies the user from the single-use `state` token
    # (like the OIDC callback), so it must work without a session cookie - an expired
    # cookie or a bearer-token client then lands on the graceful /profile?spotify=error
    # redirect instead of a raw 401.
    "/api/v1/me/connections/spotify/auth/callback",
    # MusicBrainz returns with a one-time token; only this exact callback is public.
    "/api/v1/library/contributions/musicbrainz/callback",
    # OpenAPI spec (single file)
    "/api/v1/openapi.json",
})

# Prefix matches for paths that have sub-routes (Swagger UI assets, etc.)
_PUBLIC_PREFIXES: tuple[str, ...] = (
    "/api/v1/docs",
    "/api/v1/redoc",
    "/api/v1/wrapped",
)


def _rate_limited_response(limiter: TokenBucketRateLimiter) -> MsgSpecJSONResponse:
    retry_after = limiter.retry_after()
    return MsgSpecJSONResponse(
        status_code=429,
        content={
            "error": {
                "code": "RATE_LIMITED",
                "message": "Too many requests",
                "details": None,
            }
        },
        headers={
            "Retry-After": str(int(retry_after)),
            "X-RateLimit-Limit": str(limiter.capacity),
            "X-RateLimit-Remaining": "0",
        },
    )


class RateLimitMiddleware(BaseHTTPMiddleware):
    """Per-process token-bucket rate limiter with per-path overrides."""

    def __init__(
        self,
        app: ASGIApp,
        default_rate: float = 30.0,
        default_capacity: int = 60,
        overrides: dict[str, tuple[float, int]] | None = None,
    ):
        super().__init__(app)
        self._default = TokenBucketRateLimiter(rate=default_rate, capacity=default_capacity)
        self._overrides: list[tuple[str, TokenBucketRateLimiter]] = []
        for prefix, (rate, capacity) in (overrides or {}).items():
            self._overrides.append((prefix, TokenBucketRateLimiter(rate=rate, capacity=capacity)))

    def _get_limiter(self, path: str) -> TokenBucketRateLimiter:
        for prefix, limiter in self._overrides:
            if path.startswith(prefix):
                return limiter
        return self._default

    async def dispatch(self, request: Request, call_next):
        path = application_path(request.scope)
        if not path.startswith("/api/"):
            return await call_next(request)

        limiter = self._get_limiter(path)
        acquired = await limiter.try_acquire()

        if acquired:
            response = await call_next(request)
            # An inner layer (per-user limiter) may have set its own budget
            # headers already; never mask them with the aggregate bucket's.
            if "X-RateLimit-Limit" not in response.headers:
                response.headers["X-RateLimit-Limit"] = str(limiter.capacity)
                response.headers["X-RateLimit-Remaining"] = str(limiter.remaining)
            return response

        return _rate_limited_response(limiter)


class PerUserRateLimitMiddleware(BaseHTTPMiddleware):
    """Per-user token buckets behind AuthMiddleware.

    The global RateLimitMiddleware stays as the pre-auth backstop (it shields
    verify_token from unauthenticated floods) and the aggregate cap; this
    layer caps each authenticated user's burst rate against their own budget,
    so routine per-user spikes are absorbed without touching other users'
    budgets. It does not raise aggregate headroom: sustained all-user load is
    still governed by the shared global bucket. Wire it by adding it BEFORE
    AuthMiddleware (Starlette executes last-added-first). Buckets are
    intentionally not evicted: self-hosted user counts are tiny and a bucket
    is a few floats.
    """

    def __init__(
        self,
        app: ASGIApp,
        default_rate: float = 30.0,
        default_capacity: int = 60,
    ):
        super().__init__(app)
        self._rate = default_rate
        self._capacity = default_capacity
        self._buckets: dict[str, TokenBucketRateLimiter] = {}

    def _bucket_for(self, user_id: str) -> TokenBucketRateLimiter:
        bucket = self._buckets.get(user_id)
        if bucket is None:
            bucket = TokenBucketRateLimiter(rate=self._rate, capacity=self._capacity)
            self._buckets[user_id] = bucket
        return bucket

    async def dispatch(self, request: Request, call_next):
        path = application_path(request.scope)
        if not path.startswith("/api/"):
            return await call_next(request)
        user_id = getattr(getattr(request.state, "user", None), "id", None)
        if user_id is None:
            return await call_next(request)

        limiter = self._bucket_for(str(user_id))
        if await limiter.try_acquire():
            response = await call_next(request)
            response.headers["X-RateLimit-Limit"] = str(limiter.capacity)
            response.headers["X-RateLimit-Remaining"] = str(limiter.remaining)
            return response
        return _rate_limited_response(limiter)


class DegradationMiddleware(BaseHTTPMiddleware):
    """Initialise a per-request DegradationContext and surface results in a header."""

    async def dispatch(self, request: Request, call_next):
        init_degradation_context()
        try:
            response = await call_next(request)
            ctx = try_get_degradation_context()
            if ctx and ctx.has_degradation():
                sources = ",".join(
                    name for name, status in ctx.summary().items() if status != "ok"
                )
                if sources:
                    response.headers["X-Degraded-Services"] = sources
            return response
        finally:
            clear_degradation_context()


class PerformanceMiddleware(BaseHTTPMiddleware):
    
    def __init__(self, app: ASGIApp):
        super().__init__(app)
    
    async def dispatch(self, request: Request, call_next):
        start_time = time.perf_counter()
        response = await call_next(request)
        process_time = time.perf_counter() - start_time
        
        response.headers["X-Response-Time"] = f"{process_time:.3f}s"
        
        if process_time > SLOW_REQUEST_THRESHOLD:
            logger.warning(
                f"Slow request: {request.method} {request.url.path} "
                f"took {process_time:.2f}s"
            )
        
        return response


class AuthMiddleware(BaseHTTPMiddleware):
    """Global Bearer token validation for all /api/* routes.
 
    Non-/api/* paths (frontend SPA, static assets) are skipped entirely.
    Public API routes are allowlisted above. All others return 401 if the
    token is missing, invalid, or expired.
 
    On success, injects into request.state:
        - user: UserRecord
        - token: TokenRecord
    """
 
    async def dispatch(self, request: Request, call_next):
        path = application_path(request.scope)
 
        # Non-API paths: SPA routes, static files, favicons, etc.
        if not path.startswith("/api/") and path != "/health":
            return await call_next(request)
 
        # Allowlisted public API routes
        if self._is_public(path):
            return await call_next(request)
 
        # All other /api/* routes require a valid token
        raw_token = self._extract_bearer(request)
        if not raw_token:
            return self._unauthorized("Not authenticated")
 
        # Lazy import to avoid circular imports at module load time
        from core.dependencies.auth_providers import get_auth_service
        auth_service = get_auth_service()
 
        result = await auth_service.verify_token(raw_token)
        if result is None:
            return self._unauthorized("Invalid or expired token")
 
        user, token = result
        request.state.user = user
        request.state.token = token

        workload_gate = None
        if self._tracks_interactive_activity(path):
            from core.dependencies.service_providers import (
                get_background_workload_gate,
            )

            workload_gate = get_background_workload_gate()
            workload_gate.begin_interactive_request()

        try:
            return await call_next(request)
        finally:
            if workload_gate is not None:
                workload_gate.end_interactive_request()

    @staticmethod
    def _tracks_interactive_activity(path: str) -> bool:
        """Exclude long-lived media and SSE connections from activity tracking."""
        if path.startswith("/api/v1/stream/") or "/held-audio/" in path:
            return False
        return not path.endswith("/stream")

    @staticmethod
    def _is_public(path: str) -> bool:
        if path in _PUBLIC_PATHS:
            return True
        for prefix in _PUBLIC_PREFIXES:
            if path.startswith(prefix):
                return True
        return False

    @staticmethod
    def _extract_bearer(request: Request) -> str | None:
        # Bearer token (programmatic / API clients)
        auth = request.headers.get("Authorization", "")
        if auth.lower().startswith("bearer "):
            return auth[7:].strip() or None
        # httpOnly session cookie (browser)
        return request.cookies.get("droppedneedle_session") or None

    @staticmethod
    def _unauthorized(detail: str) -> MsgSpecJSONResponse:
        return MsgSpecJSONResponse(
            status_code = status.HTTP_401_UNAUTHORIZED,
            content = {"error": {"code": "UNAUTHORIZED", "message": detail, "details": None}},
            headers = {"WWW-Authenticate": "Bearer"},
        )


class HSTSMiddleware(BaseHTTPMiddleware):
    """Adds Strict-Transport-Security when hsts_max_age > 0 in security settings."""

    async def dispatch(self, request: Request, call_next):
        response = await call_next(request)
        is_https = (
            request.url.scheme == "https"
            or request.headers.get("x-forwarded-proto", "").lower() == "https"
        )
        if not is_https:
            return response
        from core.dependencies.cache_providers import get_preferences_service
        sec = get_preferences_service().get_security_settings()
        if sec.hsts_max_age > 0:
            value = f"max-age={sec.hsts_max_age}"
            if sec.hsts_include_subdomains:
                value += "; includeSubDomains"
            if sec.hsts_preload:
                value += "; preload"
            response.headers["Strict-Transport-Security"] = value
        return response


def _get_current_user(request: Request) -> UserRecord:
    """Extract the already verified user from request.state.

    The middleware has already validated the token by the time any route
    handler runs, so this is a zero-cost lookup with no DB call.
    """
    user = getattr(request.state, "user", None)
    if user is None:
        raise HTTPException(
            status_code = status.HTTP_401_UNAUTHORIZED,
            detail = "Not authenticated",
            headers = {"WWW-Authenticate": "Bearer"},
        )
    return user


def _get_current_admin(request: Request) -> UserRecord:
    """Like _get_current_user but also enforces admin role."""
    user = _get_current_user(request)
    if user.role != "admin":
        raise HTTPException(
            status_code = status.HTTP_403_FORBIDDEN,
            detail = "Admin access required",
        )
    return user


def _get_current_curator(request: Request) -> UserRecord:
    """Like _get_current_user but requires the admin OR trusted role - the curator
    surfaces (quality upgrades, edition pins; CollectionManagement D18)."""
    user = _get_current_user(request)
    if user.role not in ("admin", "trusted"):
        raise HTTPException(
            status_code = status.HTTP_403_FORBIDDEN,
            detail = "Admin or trusted access required",
        )
    return user


def _get_current_token(request: Request) -> TokenRecord:
    """Extract the already verified token record from request.state."""
    token = getattr(request.state, "token", None)
    if token is None:
        raise HTTPException(
            status_code = status.HTTP_401_UNAUTHORIZED,
            detail = "Not authenticated",
            headers = {"WWW-Authenticate": "Bearer"},
        )
    return token


CurrentUserDep = Annotated[UserRecord, Depends(_get_current_user)]
CurrentAdminDep = Annotated[UserRecord, Depends(_get_current_admin)]
CurrentCuratorDep = Annotated[UserRecord, Depends(_get_current_curator)]
CurrentTokenDep = Annotated[TokenRecord, Depends(_get_current_token)]
