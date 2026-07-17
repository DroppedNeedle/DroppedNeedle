"""Auth service: local login/registration, token lifecycle, setup."""

from __future__ import annotations

import asyncio, hashlib, httpx, logging, os, re, sqlite3, uuid, json
from typing import TYPE_CHECKING
import bcrypt as _bcrypt

from core.dependencies.cache_providers import get_preferences_service
from core.exceptions import AuthenticationError, RegistrationError
from infrastructure.persistence.auth_store import AuthStore, TokenRecord, UserRecord

if TYPE_CHECKING:
    from infrastructure.persistence.discovery_snapshot_store import DiscoverySnapshotStore

logger = logging.getLogger(__name__)

_PW_KEY = "password_hash"


class AuthService:
    def __init__(
        self,
        auth_store: AuthStore,
        discovery_snapshot_store: "DiscoverySnapshotStore | None" = None,
    ) -> None:
        self._store = auth_store
        self._discovery_snapshot_store = discovery_snapshot_store

    async def is_setup_required(self) -> bool:
        return not await self._store.has_any_users()

    async def create_first_admin(
        self,
        *,
        display_name: str,
        username: str,
        password: str,
        email: str | None = None,
        user_agent: str | None = None,
    ) -> tuple[UserRecord, str]:
        if not await self.is_setup_required():
            raise RegistrationError("Setup has already been completed")

        _validate_password(password)
        await _check_hibp(password)
        username, username_display = _validate_username(username)
        if await self._store.get_user_by_username(username) is not None:
            raise RegistrationError("Username already taken")
        email = _normalise_optional_email(email)

        user_id = _new_id()
        provider_id = _new_id()

        try:
            user = await self._store.create_user(
                id = user_id,
                display_name = display_name.strip(),
                role = "admin",
                email = email,
                username = username,
                username_display = username_display,
            )

            await self._store.create_auth_provider(
                id = provider_id,
                user_id = user_id,
                provider = "local",
                provider_uid = username,
                provider_data = _make_local_data(password),
            )
        except sqlite3.IntegrityError:
            # Unique-index race on username/email between the pre-check and insert.
            raise RegistrationError("Could not create account")

        raw_token = await self._issue_session(user_id, user_agent = user_agent)
        await self._store.update_last_login(user_id)

        logger.info(f"First admin account created: {display_name} ({user_id[:8]})")
        return user, raw_token

    async def admin_create_user(
        self,
        *,
        display_name: str,
        username: str,
        password: str,
        email: str | None = None,
        role: str = "user",
    ) -> UserRecord:
        if role not in ("admin", "trusted", "user"):
            raise RegistrationError(f"Invalid role: {role}")
        _validate_password(password)
        await _check_hibp(password)
        username, username_display = _validate_username(username)
        if await self._store.get_user_by_username(username) is not None:
            raise RegistrationError("Could not create user")

        email = _normalise_optional_email(email)
        if email is not None and await self._store.get_user_by_email(email) is not None:
            raise RegistrationError("Could not create user")

        user_id = _new_id()
        provider_id = _new_id()

        try:
            user = await self._store.create_user(
                id = user_id,
                display_name = display_name.strip(),
                role = role,
                email = email,
                username = username,
                username_display = username_display,
            )

            await self._store.create_auth_provider(
                id = provider_id,
                user_id = user_id,
                provider = "local",
                provider_uid = username,
                provider_data = _make_local_data(password),
            )
        except sqlite3.IntegrityError:
            # Unique-index race on username/email between the pre-check and insert.
            raise RegistrationError("Could not create user")

        logger.info(f"Admin created user: {display_name} ({user_id[:8]}) role: {role}")
        return user

    async def login_local(
        self,
        *,
        username: str,
        password: str,
        user_agent: str | None = None,
    ) -> tuple[UserRecord, str]:
        # Usernames are stored lowercased (D3); accept mixed-case input.
        username = username.strip().lower()

        user = await self._store.get_user_by_username(username)
        provider = await self._store.get_auth_provider("local", username) if user else None
        if user is None or provider is None:
            # Don't reveal whether the username exists (or has a local password).
            _dummy_verify()
            raise AuthenticationError("Invalid username or password")

        if not _verify_password(password, provider.provider_data or ""):
            raise AuthenticationError("Invalid username or password")

        raw_token = await self._issue_session(user.id, user_agent = user_agent)
        await self._store.update_last_login(user.id)

        logger.info(f"Local login: {user.display_name} ({user.id[:8]})")
        return user, raw_token

    async def update_display_name(self, user_id: str, display_name: str) -> UserRecord:
        """Self-service display_name change (D8). Persists to the auth_users row."""
        name = (display_name or "").strip()
        if not name:
            raise RegistrationError("Display name cannot be empty")
        await self._store.update_user_profile(user_id, display_name = name)
        return await self._require_user(user_id)

    async def update_avatar(self, user_id: str, avatar_url: str) -> UserRecord:
        """Persist a user's per-user avatar URL (D9) and return the fresh record."""
        await self._store.update_user_profile(user_id, avatar_url = avatar_url)
        return await self._require_user(user_id)

    async def update_username(self, user_id: str, new_username: str) -> UserRecord:
        """Self-service username change (D8), atomically syncing the local provider_uid (M3).

        Maps a unique-index collision to a domain error instead of a 500.
        """
        username, username_display = _validate_username(new_username)
        providers = await self._store.list_providers_for_user(user_id)
        local = next((p for p in providers if p.provider == "local"), None)
        try:
            await self._store.update_username(
                user_id,
                username,
                username_display,
                local_provider_id = local.id if local else None,
            )
        except sqlite3.IntegrityError:
            raise RegistrationError("Username already taken")
        return await self._require_user(user_id)

    async def update_email(self, user_id: str, new_email: str | None) -> UserRecord:
        """Self-service email change (D8). Empty/None clears to NULL; otherwise dedupes."""
        email = _normalise_optional_email(new_email)
        if email is not None:
            existing = await self._store.get_user_by_email(email)
            if existing is not None and existing.id != user_id:
                raise RegistrationError("Email already in use")
        try:
            await self._store.update_email(user_id, email)
        except sqlite3.IntegrityError:
            raise RegistrationError("Email already in use")
        return await self._require_user(user_id)

    async def change_password(
        self, user_id: str, current_password: str, new_password: str
    ) -> UserRecord:
        """Change the password of a local account (D8): verify current, then re-hash."""
        local = await self._local_provider(user_id)
        if local is None:
            raise AuthenticationError("No local password is set for this account")
        if not _verify_password(current_password, local.provider_data or ""):
            raise AuthenticationError("Current password is incorrect")
        _validate_password(new_password)
        await _check_hibp(new_password)
        await self._store.update_provider_data(local.id, _make_local_data(new_password))
        return await self._require_user(user_id)

    async def set_local_password(self, user_id: str, new_password: str) -> UserRecord:
        """Add a local password to an SSO-only account (D8).

        Binds provider_uid to the user's username (guaranteed non-NULL after Phase 1's
        SSO auto-derive/backfill) so login_local resolves afterwards. Rejects accounts
        that already have a local provider - those use change_password instead.
        """
        if await self._local_provider(user_id) is not None:
            raise RegistrationError("A local password already exists; use change password instead")
        user = await self._require_user(user_id)
        if not user.username:
            raise RegistrationError("Choose a username first")
        _validate_password(new_password)
        await _check_hibp(new_password)
        try:
            await self._store.create_auth_provider(
                id = _new_id(),
                user_id = user_id,
                provider = "local",
                provider_uid = user.username,
                provider_data = _make_local_data(new_password),
            )
        except sqlite3.IntegrityError:
            raise RegistrationError("Could not set a local password")
        return user

    async def _local_provider(self, user_id: str):
        providers = await self._store.list_providers_for_user(user_id)
        return next((p for p in providers if p.provider == "local"), None)

    async def _require_user(self, user_id: str) -> UserRecord:
        user = await self._store.get_user_by_id(user_id)
        if user is None:
            raise AuthenticationError("User not found")
        return user

    async def verify_token(self, raw_token: str) -> tuple[UserRecord, TokenRecord] | None:
        token = await self._store.verify_token(raw_token)
        if token is None:
            return None

        user = await self._store.get_user_by_id(token.user_id)
        if user is None:
            return None

        try:
            await self._store.touch_token(token.id)
        except Exception:  # noqa: BLE001
            pass

        return user, token

    async def logout(self, raw_token: str) -> None:
        token = await self._store.verify_token(raw_token)
        if token is not None:
            await self._store.revoke_token(token.id)

    async def logout_all(self, user_id: str, *, except_raw_token: str | None = None, except_token_id: str | None = None) -> None:
        if except_token_id is None and except_raw_token:
            current = await self._store.verify_token(except_raw_token)
            if current:
                except_token_id = current.id

        await self._store.revoke_all_tokens_for_user(user_id, except_token_id = except_token_id)

    async def list_users(self, limit: int = 100, offset: int = 0) -> list[UserRecord]:
        return await self._store.list_users(limit = limit, offset = offset)

    async def count_users(self) -> int:
        return await self._store.count_users()

    async def set_role(self, user_id: str, role: str, *, requesting_user_id: str | None = None) -> None:
        if role not in ("admin", "trusted", "user"):
            raise AuthenticationError(f"Invalid role: {role}")
        if requesting_user_id is not None and requesting_user_id == user_id and role != "admin":
            raise AuthenticationError("Cannot remove your own admin privileges")
        target = await self._store.get_user_by_id(user_id)
        if target is not None and target.role == "admin" and role != "admin":
            admin_count = await self._store.count_users_by_role("admin")
            if admin_count <= 1:
                raise AuthenticationError("Cannot remove the last admin account")
        await self._store.update_user_role(user_id, role)

    async def delete_user(self, user_id: str, *, requesting_user_id: str) -> None:
        if requesting_user_id == user_id:
            raise AuthenticationError("Cannot delete your own account")
        target = await self._store.get_user_by_id(user_id)
        if target is None:
            raise AuthenticationError("User not found")
        if target.role == "admin":
            admin_count = await self._store.count_users_by_role("admin")
            if admin_count <= 1:
                raise AuthenticationError("Cannot delete the last admin account")
        if self._discovery_snapshot_store is not None:
            await self._discovery_snapshot_store.delete_user(user_id)
        await self._store.delete_user(user_id)

    async def get_provider_names_for_users(self, user_ids: list[str]) -> dict[str, list[str]]:
        return await self._store.list_provider_names_for_users(user_ids)

    async def revoke_user_sessions(self, user_id: str) -> None:
        await self._store.revoke_all_tokens_for_user(user_id)

    async def list_sessions(self, user_id: str) -> list[TokenRecord]:
        return await self._store.list_tokens_for_user(user_id)

    async def revoke_session(self, token_id: str, requesting_user_id: str) -> None:
        tokens = await self._store.list_tokens_for_user(requesting_user_id)
        owned = any(token.id == token_id for token in tokens)
        if not owned:
            raise AuthenticationError("Cannot revoke a session that does not belong to you")
        await self._store.revoke_token(token_id)

    async def cleanup_expired_tokens(self) -> int:
        return await self._store.cleanup_expired_tokens()

    async def _issue_session(self, user_id: str, *, user_agent: str | None = None) -> str:
        raw_token, token_hash = self._store.issue_token()
        await self._store.store_token(
            id = _new_id(),
            user_id = user_id,
            token_hash = token_hash,
            user_agent = user_agent,
        )
        return raw_token


def _new_id() -> str:
    return str(uuid.uuid4())


def _make_local_data(password: str) -> str:
    """Return a JSON-safe string holding the bcrypt hash.

    External tokens (Plex, Jellyfin) will use encrypted JSON in provider_data.
    For local accounts we just store the bcrypt hash directly, bcrypt is
    already a one-way function designed for password storage, encryption
    on top adds no meaningful security benefit.
    """
    hashed = _bcrypt.hashpw(password.encode(), _bcrypt.gensalt()).decode()
    return json.dumps({_PW_KEY: hashed})


def _verify_password(password: str, provider_data: str) -> bool:
    try:
        data = json.loads(provider_data)
        stored_hash = data.get(_PW_KEY, "")
        return _bcrypt.checkpw(password.encode(), stored_hash.encode())
    except Exception:  # noqa: BLE001
        return False


def _dummy_verify() -> None:
    """Run a bcrypt verify against a dummy hash to prevent timing attacks
    that would reveal whether an email address is registered."""
    try:
        _bcrypt.checkpw(
            b"dummy",
            b"$2b$12$KIXqKFZb9VpLJ3DFnvOHEeGjF1f8L4RkX5p7Z2YqM9U3J0BwN1C6K",
        )
    except Exception:  # noqa: BLE001
        pass


def _validate_password(password: str) -> None:
    if len(password) < 12:
        raise RegistrationError("Password must be at least 12 characters")


def _validate_email(email: str) -> None:
    if not email or "@" not in email or len(email) < 5:
        raise RegistrationError("Invalid email address")


_USERNAME_RE = re.compile(r"^[a-zA-Z0-9._-]+$")


def _validate_username(username: str) -> tuple[str, str]:
    """Validate a username (D3): mixed-case input, charset [a-zA-Z0-9._-], 3-32 chars.

    Returns (lowercased_for_storage, original_casing_for_display).
    """
    candidate = (username or "").strip()
    if not (3 <= len(candidate) <= 32) or _USERNAME_RE.match(candidate) is None:
        raise RegistrationError("Invalid username")
    return candidate.lower(), candidate


def _normalise_optional_email(email: str | None) -> str | None:
    """Lowercase + validate an email when supplied; treat blank/None as absent (D3)."""
    if not email or not email.strip():
        return None
    normalised = email.lower().strip()
    _validate_email(normalised)
    return normalised


async def _check_hibp(password: str) -> None:
    """Reject passwords found in the Have I Been Pwned breach corpus.

    Priority:
      1. Local file (if hibp_local_path is set and the file exists), no outbound calls.
      2. api.pwnedpasswords.com range API, free, no key required, uses k-anonymity
         so only the first 5 hex chars of the SHA-1 hash leave the server.
         Note: this is separate from the paid haveibeenpwned.com notification API.
    If hibp_check is False, the function returns immediately.
    Both paths fail open on error so a missing file or unreachable service never
    blocks account creation.
    """
    sec = get_preferences_service().get_security_settings()

    if not sec.hibp_check:
        return

    sha1 = hashlib.sha1(password.encode("utf-8")).hexdigest().upper()

    if sec.hibp_local_path:
        path = sec.hibp_local_path.strip()
        if os.path.isfile(path):
            found = await asyncio.to_thread(_search_hibp_file, sha1, path)
            if found:
                raise RegistrationError(
                    "This password has appeared in a known data breach. Please choose a different password."
                )
            return
        else:
            logger.warning("HIBP local path configured but file not found. Skipping breach check")
            return  # user opted out of API calls; fail open

    # --- api.pwnedpasswords.com range API (k-anonymity, free, no key required) ---
    # Distinct from the paid haveibeenpwned.com notification/lookup API.
    # Only the first 5 hex chars of the SHA-1 hash are transmitted.
    prefix, suffix = sha1[:5], sha1[5:]
    try:
        async with httpx.AsyncClient(timeout=5.0) as client:
            resp = await client.get(
                f"https://api.pwnedpasswords.com/range/{prefix}",
                headers={"Add-Padding": "true"},
            )
        if resp.status_code != 200:
            return  # fail open

        for line in resp.text.splitlines():
            if ":" not in line:
                continue
            line_suffix, _ = line.split(":", 1)
            if line_suffix.upper() == suffix:
                raise RegistrationError(
                    "This password has appeared in a known data breach. Please choose a different password."
                )
    except RegistrationError:
        raise
    except Exception:  # noqa: BLE001
        logger.warning("pwnedpasswords.com range check failed (network error). Proceeding without breach check")


def _search_hibp_file(sha1: str, path: str) -> bool:
    """Binary-search a sorted HIBP 'Pwned Passwords (ordered by hash)' text file.

    File format (official HIBP download, hash-ordered):
        ABCDEF...40HEX:COUNT\\r\\n   (one entry per line, sorted ascending)

    O(log N) disk seeks, typically ~30 seeks regardless of file size.
    """
    try:
        file_size = os.path.getsize(path)
        if file_size == 0:
            return False

        with open(path, "rb") as fh:
            low: int = 0
            high: int = file_size

            while low < high:
                mid = (low + high) // 2
                fh.seek(mid)

                # Skip the partial line we landed in the middle of.
                if mid > 0:
                    fh.readline()

                line_start = fh.tell()
                if line_start >= high:
                    break

                raw = fh.readline()
                if not raw:
                    break

                text = raw.decode("ascii", errors="ignore").rstrip("\r\n")
                if not text or ":" not in text:
                    break

                line_hash = text.split(":", 1)[0].upper()

                if line_hash == sha1:
                    return True
                elif line_hash < sha1:
                    # Target is after this line.
                    new_low = fh.tell()
                    if new_low <= low:
                        break  # no progress, file may be malformed
                    low = new_low
                else:
                    # Target (if present) is before this line's position.
                    if mid <= low:
                        break
                    high = mid

        return False
    except OSError:
        return False
