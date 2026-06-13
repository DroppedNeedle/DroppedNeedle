"""OIDC user authentication service"""

from __future__ import annotations

import hashlib, json, logging, os, uuid
from urllib.parse import urlencode

import httpx

from base64 import urlsafe_b64decode, urlsafe_b64encode
from typing import Any

from api.v1.schemas.auth import AuthProvidersResponse
from api.v1.schemas.settings import OIDCConnectionSettings
from core.exceptions import AuthenticationError, ConfigurationError, ExternalServiceError
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.crypto import encrypt
from infrastructure.persistence.auth_store import AuthStore, UserRecord

logger = logging.getLogger(__name__)

_DISCOVERY_TTL = 86_400
_CODE_TTL = 60
_STATE_TTL = 600

_CACHE_PREFIX_DISCOVERY = "oidc:discovery:"
_CACHE_PREFIX_CODE      = "oidc:code:"
_CACHE_PREFIX_PKCE      = "oidc:pkce:"


class OIDCUserAuthService:
    def __init__(
        self,
        auth_store: AuthStore,
        preferences_service,
        cache: CacheInterface,
    ) -> None:
        self._store  = auth_store
        self._prefs  = preferences_service
        self._cache  = cache

    def get_config(self) -> OIDCConnectionSettings:
        return self._prefs.get_oidc_connection()

    def get_enabled_providers(self) -> AuthProvidersResponse:
        jellyfin_cfg = self._prefs.get_jellyfin_connection()
        plex_cfg = self._prefs.get_plex_connection()
        oidc_cfg = self.get_config()

        return AuthProvidersResponse(
            local = True,
            plex = plex_cfg.login_enabled,
            jellyfin = jellyfin_cfg.login_enabled,
            oidc = oidc_cfg.enabled and bool(oidc_cfg.issuer) and bool(oidc_cfg.client_id),
        )

    async def verify(self, settings: OIDCConnectionSettings) -> tuple[bool, str]:
        issuer = settings.issuer.strip()
        if not issuer:
            return False, "Issuer URL is required"
        try:
            doc = await self._discover(issuer)
        except (ExternalServiceError, ConfigurationError) as e:
            return False, str(e)
        return True, f"Connected to {doc.get('issuer', issuer)}"

    def _require_config(self) -> OIDCConnectionSettings:
        config = self._prefs.get_oidc_connection_raw()
        if not config.enabled:
            raise ConfigurationError("OIDC login is not enabled")
        if not all([config.issuer, config.client_id, config.redirect_uri]):
            raise ConfigurationError("OIDC configuration is incomplete")
        return config

    @staticmethod
    def _normalise_claims(claims: dict) -> dict:
        sub = claims.get("sub", "")
        email = claims.get("email", "") or ""
        name = (
            claims.get("name")
            or claims.get("preferred_username")
            or claims.get("nickname")
            or email.split("@")[0]
            or "OIDC User"
        )
        thumb = claims.get("picture") or claims.get("avatar") or None

        if not sub:
            raise AuthenticationError("OIDC token missing 'sub' claim")

        return {
            "sub": sub,
            "email": email.lower().strip() if email else None,
            "name": name,
            "thumb": thumb,
        }

    async def _discover(self, issuer: str) -> dict[str, Any]:
        cache_key = f"{_CACHE_PREFIX_DISCOVERY}{hashlib.sha256(issuer.encode()).hexdigest()[:16]}"
        cached = await self._cache.get(cache_key)
        if cached:
            return cached

        url = issuer.rstrip("/") + "/.well-known/openid-configuration"
        try:
            async with httpx.AsyncClient(timeout = 10.0) as client:
                resp = await client.get(url)
            if resp.status_code != 200:
                raise ExternalServiceError("Failed to fetch OIDC discovery document")
            doc = resp.json()
        except httpx.HTTPError as e:
            logger.debug(f"OIDC discovery fetch failed: {e}")
            raise ExternalServiceError("Could not reach OIDC provider")

        required = {"authorization_endpoint", "token_endpoint"}
        missing = required - set(doc.keys())
        if missing:
            raise ConfigurationError(f"OIDC discovery document missing: {missing}")

        await self._cache.set(cache_key, doc, ttl_seconds = _DISCOVERY_TTL)
        logger.info(f"OIDC discovery cached for issuer {issuer[:40]}")
        return doc

    async def build_authorize_url(self) -> str:
        config = self._require_config()
        doc = await self._discover(config.issuer)

        state = _random_state()
        await self._store.store_oidc_state(state, ttl_seconds = _STATE_TTL)

        # PKCE (S256): stash the verifier against the state so the callback can
        # complete the token exchange. Lets public clients (no secret) work, and
        # is recommended even for confidential clients (OAuth 2.1).
        verifier, challenge = _make_pkce()
        await self._cache.set(
            f"{_CACHE_PREFIX_PKCE}{state}", verifier, ttl_seconds = _STATE_TTL
        )

        params = {
            "response_type": "code",
            "client_id": config.client_id,
            "redirect_uri": config.redirect_uri,
            "scope": config.scopes,
            "state": state,
            "code_challenge": challenge,
            "code_challenge_method": "S256",
        }
        query = urlencode(params)
        return f"{doc['authorization_endpoint']}?{query}"

    async def handle_callback(self, *, code: str, state: str, user_agent: str | None = None) -> str:
        valid = await self._store.consume_oidc_state(state)
        if not valid:
            raise AuthenticationError("Invalid or expired OIDC state")

        pkce_key = f"{_CACHE_PREFIX_PKCE}{state}"
        code_verifier = await self._cache.get(pkce_key)
        await self._cache.delete(pkce_key)

        config = self._require_config()
        doc = await self._discover(config.issuer)

        tokens = await self._exchange_code(code, config, doc, code_verifier)

        profile = await self._fetch_user_info(tokens, config, doc)

        user = await self._find_or_create_user(profile, tokens)

        raw_token, token_hash = self._store.issue_token()
        await self._store.store_token(
            id = str(uuid.uuid4()),
            user_id = user.id,
            token_hash = token_hash,
            user_agent = user_agent,
        )
        await self._store.update_last_login(user.id)

        exchange_code = _random_state()
        cache_key = f"{_CACHE_PREFIX_CODE}{exchange_code}"
        await self._cache.set(
            cache_key,
            {"token": raw_token, "user_id": user.id},
            ttl_seconds = _CODE_TTL,
        )

        logger.info("OIDC login: %s (%s)", user.display_name, user.id[:8])
        return exchange_code

    async def exchange_code(self, exchange_code: str) -> tuple[UserRecord, str]:
        cache_key = f"{_CACHE_PREFIX_CODE}{exchange_code}"
        data = await self._cache.get(cache_key)
        if not data:
            raise AuthenticationError("Invalid or expired exchange code")

        await self._cache.delete(cache_key)

        raw_token = data["token"]
        user_id = data["user_id"]

        user = await self._store.get_user_by_id(user_id)
        if user is None:
            raise AuthenticationError("User not found")

        return user, raw_token

    async def _exchange_code(
        self,
        code: str,
        config: OIDCConnectionSettings,
        doc: dict,
        code_verifier: str | None = None,
    ) -> dict:
        body = {
            "grant_type": "authorization_code",
            "code": code,
            "redirect_uri": config.redirect_uri,
            "client_id": config.client_id,
        }
        # Confidential clients still send their secret; public (PKCE-only) clients
        # don't have one. PKCE proves possession of the verifier for both.
        if config.client_secret:
            body["client_secret"] = config.client_secret
        if code_verifier:
            body["code_verifier"] = code_verifier
        try:
            async with httpx.AsyncClient(timeout = 15.0) as client:
                resp = await client.post(
                    doc["token_endpoint"],
                    data = body,
                    headers = {"Accept": "application/json"},
                )
        except httpx.HTTPError as e:
            logger.debug(f"OIDC token exchange failed: {e}")
            raise ExternalServiceError("Could not reach OIDC provider")

        if resp.status_code not in (200, 201):
            logger.debug(f"OIDC token endpoint returned {resp.status_code}")
            raise AuthenticationError("OIDC token exchange failed")

        try:
            return resp.json()
        except Exception:  # noqa: BLE001
            raise ExternalServiceError("OIDC provider returned an unexpected response")

    async def _fetch_user_info(self, tokens: dict, config: OIDCConnectionSettings, doc: dict) -> dict:
        access_token = tokens.get("access_token", "")

        userinfo_endpoint = doc.get("userinfo_endpoint")
        if userinfo_endpoint and access_token:
            try:
                async with httpx.AsyncClient(timeout = 10.0) as client:
                    resp = await client.get(
                        userinfo_endpoint,
                        headers = {"Authorization": f"Bearer {access_token}"},
                    )
                if resp.status_code == 200:
                    return self._normalise_claims(resp.json())
            except Exception as e:  # noqa: BLE001
                logger.debug(f"OIDC userinfo fetch failed, falling back to id_token: {e}")

        id_token = tokens.get("id_token", "")
        if id_token:
            claims = _decode_jwt_payload(id_token)
            if claims:
                return self._normalise_claims(claims)

        raise AuthenticationError("Could not retrieve user info from OIDC provider")

    async def _find_or_create_user(self, profile: dict, tokens: dict) -> UserRecord:
        oidc_uid = profile["sub"]
        email = profile["email"]
        name = profile["name"]
        thumb = profile["thumb"]

        provider_data = encrypt(json.dumps({
            "access_token": tokens.get("access_token", ""),
            "refresh_token": tokens.get("refresh_token", ""),
        }))

        existing_provider = await self._store.get_auth_provider("oidc", oidc_uid)
        if existing_provider:
            await self._store.update_provider_data(existing_provider.id, provider_data)
            user = await self._store.get_user_by_id(existing_provider.user_id)
            if user is None:
                raise AuthenticationError("Linked account not found")
            return user

        if email:
            existing_user = await self._store.get_user_by_email(email)
            if existing_user:
                await self._store.create_auth_provider(
                    id = str(uuid.uuid4()),
                    user_id = existing_user.id,
                    provider = "oidc",
                    provider_uid = oidc_uid,
                    provider_data = provider_data,
                )
                logger.info(f"Linked OIDC account to existing user: {existing_user.display_name} ({existing_user.id[:8]})")
                return existing_user

        user_id = str(uuid.uuid4())
        provider_id = str(uuid.uuid4())
        is_first = not await self._store.has_any_users()

        user = await self._store.create_user(
            id = user_id,
            display_name = name,
            role = "admin" if is_first else "user",
            email = email,
            avatar_url = thumb,
        )
        await self._store.create_auth_provider(
            id = provider_id,
            user_id = user_id,
            provider = "oidc",
            provider_uid = oidc_uid,
            provider_data = provider_data,
        )
        logger.info(f"New user created via OIDC: {name} ({user_id[:8]}) role = {user.role}")
        return user


def _random_state() -> str:
    return urlsafe_b64encode(os.urandom(32)).decode()


def _make_pkce() -> tuple[str, str]:
    """Return (code_verifier, code_challenge) for PKCE S256.

    The verifier is 43 chars of base64url (unreserved per RFC 7636); the
    challenge is base64url(SHA256(verifier)), both unpadded.
    """
    verifier = urlsafe_b64encode(os.urandom(32)).rstrip(b"=").decode()
    challenge = urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b"=").decode()
    return verifier, challenge


def _decode_jwt_payload(token: str) -> dict | None:
    try:
        parts = token.split(".")
        if len(parts) != 3:
            return None
        payload = parts[1]
        payload += "=" * (4 - len(payload) % 4)
        decoded = urlsafe_b64decode(payload)
        return json.loads(decoded)
    except Exception:  # noqa: BLE001
        return None
