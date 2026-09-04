"""Plex user authentication service"""

from __future__ import annotations

import json, logging, sqlite3, uuid

from core.exceptions import AuthenticationError, PlexApiError
from infrastructure.crypto import encrypt
from infrastructure.persistence.auth_store import AuthStore, UserRecord, _derive_username

logger = logging.getLogger(__name__)

_PRODUCT = "DroppedNeedle"


class PlexUserAuthService:
    def __init__(
        self,
        auth_store: AuthStore,
        plex_repository,
        preferences_service,
        connections_store=None,
        cache=None,
    ) -> None:
        self._store = auth_store
        self._plex_repo = plex_repository
        self._prefs = preferences_service
        self._connections_store = connections_store
        self._cache = cache

    def get_client_id(self) -> str:
        return self._prefs.get_or_create_setting("plex_client_id", lambda: str(uuid.uuid4()))

    async def create_login_pin(self) -> tuple[int, str]:
        client_id = self.get_client_id()
        try:
            pin = await self._plex_repo.create_oauth_pin(client_id)
        except PlexApiError as e:
            logger.error(f"Failed to create Plex OAuth pin: {e}")
            raise AuthenticationError("Could not start Plex authentication")

        auth_url = (
            f"https://app.plex.tv/auth#?"
            f"clientID={client_id}"
            f"&code={pin.code}"
            f"&context%5Bdevice%5D%5Bproduct%5D={_PRODUCT}"
        )
        return pin.id, auth_url

    async def poll_and_login(self, pin_id: int, user_agent: str | None = None) -> tuple[UserRecord, str] | None:
        client_id = self.get_client_id()
        auth_token = await self._plex_repo.poll_oauth_pin(pin_id, client_id)
        if not auth_token:
            return None

        profile = await self._get_user_profile(auth_token, client_id)

        machine_id = await self._get_server_machine_id()
        if machine_id:
            if not await self._check_server_membership(auth_token, client_id, machine_id):
                logger.warning(f"Plex login rejected: user {profile.get('uuid', '?')[:8]} not on server {machine_id[:8]}")
                raise AuthenticationError("Your Plex account does not have access to this server")
            profile["server_access_token"] = await self._get_server_access_token(
                auth_token, client_id, machine_id
            )

        user = await self._find_or_create_user(profile, auth_token)

        # auto-link the per-user media connection (D4): the login just handed us a
        # fresh user-scoped token, so playback attribution works with zero setup
        await self._auto_link_connection(user.id, profile)

        raw_token, token_hash = self._store.issue_token()
        await self._store.store_token(
            id = str(uuid.uuid4()),
            user_id = user.id,
            token_hash = token_hash,
            user_agent = user_agent,
        )
        await self._store.update_last_login(user.id)

        logger.info(f"Plex login: {user.display_name} ({user.id[:8]})")
        return user, raw_token

    async def poll_for_link(self, pin_id: int) -> dict | None:
        """Poll a link-flow pin. Returns the verified profile (uuid /
        display_name / auth_token) once the user authorizes, ``None`` while the
        pin is still pending. Enforces the same server-membership gate as login;
        no DroppedNeedle login side effects."""
        client_id = self.get_client_id()
        auth_token = await self._plex_repo.poll_oauth_pin(pin_id, client_id)
        if not auth_token:
            return None

        profile = await self._get_user_profile(auth_token, client_id)

        machine_id = await self._get_server_machine_id()
        if not machine_id:
            raise AuthenticationError("Could not verify the configured Plex server")
        if not await self._check_server_membership(auth_token, client_id, machine_id):
            logger.warning(f"Plex link rejected: user {profile.get('uuid', '?')[:8]} not on server {machine_id[:8]}")
            raise AuthenticationError("Your Plex account does not have access to this server")
        profile["server_access_token"] = await self._get_server_access_token(
            auth_token, client_id, machine_id
        )

        return profile

    async def _auto_link_connection(self, user_id: str, profile: dict) -> None:
        if self._connections_store is None:
            return
        try:
            await self._connections_store.upsert(
                user_id,
                "plex",
                {
                    "auth_token": profile["auth_token"],
                    "server_access_token": profile.get("server_access_token", ""),
                    "plex_user_id": profile["uuid"],
                    "username": profile["display_name"],
                },
            )
            if self._cache is not None:
                from services.media_playlist_cache import invalidate_media_playlist_cache

                await invalidate_media_playlist_cache(self._cache, user_id, "plex")
        except Exception:  # noqa: BLE001 - a failed auto-link must never fail the login
            logger.warning(
                f"Failed to auto-link Plex connection for user {user_id[:8]}", exc_info=True
            )

    async def _get_user_profile(self, auth_token: str, client_id: str) -> dict:
        try:
            profile = await self._plex_repo.get_account_profile(auth_token, client_id)
        except PlexApiError as e:
            logger.error(f"Failed to fetch Plex user profile: {e}")
            raise AuthenticationError("Could not verify Plex account")

        return {
            "uuid": profile.uuid,
            "email": profile.email or "",
            "display_name": profile.display_name,
            "thumb": profile.thumb,
            "auth_token": auth_token,
        }

    async def _check_server_membership(self, auth_token: str, client_id: str, machine_id: str) -> bool:
        try:
            server_ids = await self._plex_repo.get_account_server_ids(auth_token, client_id)
        except PlexApiError as e:
            logger.error(f"Failed to fetch Plex resources: {e}")
            raise AuthenticationError("Could not verify server access")

        return machine_id in server_ids

    async def _get_server_access_token(
        self, auth_token: str, client_id: str, machine_id: str
    ) -> str:
        try:
            token = await self._plex_repo.get_server_access_token(
                auth_token, client_id, machine_id
            )
        except PlexApiError as e:
            logger.error(f"Failed to resolve Plex server token: {e}")
            raise AuthenticationError("Could not verify Plex server access")
        if not token:
            raise AuthenticationError("Could not verify Plex server access")
        return token

    async def _get_server_machine_id(self) -> str | None:
        try:
            plex_settings = self._prefs.get_plex_connection_raw()
            if not plex_settings.enabled:
                return None
            machine_id = await self._plex_repo.get_machine_identifier()
            return machine_id
        except Exception as e:  # noqa: BLE001
            logger.warning(f"Could not get Plex server machine ID: {e}")
            return None

    async def _find_or_create_user(self, profile: dict, auth_token: str) -> UserRecord:
        plex_uid = profile["uuid"]
        email = profile["email"] or None
        name = profile["display_name"]
        thumb = profile["thumb"]

        provider_data = encrypt(json.dumps({"auth_token": auth_token}))

        existing_provider = await self._store.get_auth_provider("plex", plex_uid)
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
                    provider = "plex",
                    provider_uid = plex_uid,
                    provider_data = provider_data,
                )
                logger.info(f"Linked Plex account to existing user: {existing_user.display_name} ({existing_user.id[:8]})")
                return existing_user

        user_id = str(uuid.uuid4())
        provider_id = str(uuid.uuid4())
        is_first = not await self._store.has_any_users()

        # Auto-derive a username from the Plex display name (D3) so an SSO-only
        # account can later set a local password without choosing a username.
        # Retry on the unique-index race so two concurrent first-logins whose names
        # slug to the same base don't 500 (mirrors AuthStore._assign_unique_username).
        user = None
        for _attempt in range(20):
            derived_username, derived_display = await _derive_username(self._store, display_name = name)
            try:
                user = await self._store.create_user(
                    id = user_id,
                    display_name = name,
                    role = "admin" if is_first else "user",
                    email = email,
                    avatar_url = thumb,
                    username = derived_username,
                    username_display = derived_display,
                )
                break
            except sqlite3.IntegrityError:
                continue
        if user is None:
            raise AuthenticationError("Could not create an account from Plex")
        await self._store.create_auth_provider(
            id = provider_id,
            user_id = user_id,
            provider = "plex",
            provider_uid = plex_uid,
            provider_data = provider_data,
        )
        logger.info(f"New user created via Plex: {name} ({user_id[:8]}) role = {user.role}")
        return user
