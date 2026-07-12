import time

import msgspec

from core.exceptions import ConfigurationError
from repositories.protocols import LastFmRepositoryProtocol

MAX_PENDING_TOKENS = 5
TOKEN_TTL_SECONDS = 600

LASTFM_AUTH_URL = "https://www.last.fm/api/auth/"


class TokenEntry(msgspec.Struct):
    token: str
    created_at: float


class LastFmAuthService:
    def __init__(self, lastfm_repo: LastFmRepositoryProtocol, auth_url: str = LASTFM_AUTH_URL):
        self._repo = lastfm_repo
        # admin-configurable auth root for Last.fm-compatible services
        # (libre.fm etc.); official www.last.fm default
        self._auth_url = auth_url or LASTFM_AUTH_URL
        self._pending_tokens: dict[str, TokenEntry] = {}

    def _evict_expired(self) -> None:
        now = time.time()
        expired = [
            k for k, v in self._pending_tokens.items()
            if now - v.created_at > TOKEN_TTL_SECONDS
        ]
        for k in expired:
            del self._pending_tokens[k]

    async def request_token(self, api_key: str) -> tuple[str, str]:
        self._evict_expired()

        if len(self._pending_tokens) >= MAX_PENDING_TOKENS:
            oldest_key = min(self._pending_tokens, key=lambda k: self._pending_tokens[k].created_at)
            del self._pending_tokens[oldest_key]

        result = await self._repo.get_token()
        token = result.token

        self._pending_tokens[token] = TokenEntry(token=token, created_at=time.time())

        auth_url = f"{self._auth_url}?api_key={api_key}&token={token}"
        return token, auth_url

    async def exchange_session(self, token: str) -> tuple[str, str, str]:
        self._evict_expired()

        if token not in self._pending_tokens:
            raise ConfigurationError(
                "Token expired or not recognized. Please restart the authorization flow."
            )

        result = await self._repo.get_session(token)

        self._pending_tokens.pop(token, None)

        return result.name, result.key, ""
