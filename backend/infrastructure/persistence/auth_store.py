"""Auth persistence store: users, auth_providers, tokens tables."""

from __future__ import annotations

import asyncio, hashlib, hmac, logging, os, sqlite3, threading
from base64 import urlsafe_b64encode
from datetime import datetime, timedelta, timezone
from pathlib import Path

import msgspec

logger = logging.getLogger(__name__)

TOKEN_BYTES = 32
TOKEN_LIFETIME_DAYS = 30


class UserRecord(msgspec.Struct, frozen = True):
    id: str
    display_name: str
    role: str
    created_at: str
    last_login_at: str | None = None
    email: str | None = None
    avatar_url: str | None = None


class AuthProviderRecord(msgspec.Struct, frozen = True):
    id: str
    user_id: str
    provider: str
    provider_uid: str
    created_at: str
    provider_data: str | None = None


class TokenRecord(msgspec.Struct, frozen = True):
    id: str
    user_id: str
    token_hash: str
    issued_at: str
    expires_at: str
    last_seen_at: str
    revoked: bool
    user_agent: str | None = None


class AuthStore:
    """SQLite-backed store for auth state.

    Shares the same db_path and write_lock as all other persistence stores
    so it operates on a single WAL-mode database file.
    """

    def __init__(self, db_path: Path, write_lock: threading.Lock | None = None) -> None:
        self.db_path = Path(db_path)
        self.db_path.parent.mkdir(parents = True, exist_ok = True)
        self._write_lock = write_lock or threading.Lock()
        with self._write_lock:
            self._ensure_tables()

    def _connect(self) -> sqlite3.Connection:
        conn = sqlite3.connect(self.db_path, check_same_thread = False)
        conn.row_factory = sqlite3.Row
        conn.execute("PRAGMA journal_mode=WAL")
        conn.execute("PRAGMA synchronous=NORMAL")
        conn.execute("PRAGMA foreign_keys=ON")
        return conn

    def _execute(self, operation, write: bool):
        if write:
            with self._write_lock:
                conn = self._connect()
                try:
                    result = operation(conn)
                    conn.commit()
                    return result
                finally:
                    conn.close()
        conn = self._connect()
        try:
            return operation(conn)
        finally:
            conn.close()

    async def _read(self, operation):
        return await asyncio.to_thread(self._execute, operation, False)

    async def _write(self, operation):
        return await asyncio.to_thread(self._execute, operation, True)

    def _ensure_tables(self) -> None:
        conn = self._connect()
        try:
            conn.executescript("""
                CREATE TABLE IF NOT EXISTS auth_users (
                    id TEXT PRIMARY KEY,
                    display_name TEXT NOT NULL,
                    email TEXT UNIQUE,
                    avatar_url TEXT,
                    role TEXT NOT NULL DEFAULT 'user',
                    created_at TEXT NOT NULL,
                    last_login_at TEXT
                );

                CREATE TABLE IF NOT EXISTS auth_providers (
                    id TEXT PRIMARY KEY,
                    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
                    provider TEXT NOT NULL,
                    provider_uid TEXT NOT NULL,
                    provider_data TEXT,
                    created_at TEXT NOT NULL,
                    UNIQUE (provider, provider_uid)
                );
                CREATE INDEX IF NOT EXISTS idx_auth_providers_user
                    ON auth_providers(user_id);

                CREATE TABLE IF NOT EXISTS auth_tokens (
                    id TEXT PRIMARY KEY,
                    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
                    token_hash TEXT NOT NULL UNIQUE,
                    issued_at TEXT NOT NULL,
                    expires_at TEXT NOT NULL,
                    last_seen_at TEXT NOT NULL,
                    revoked INTEGER NOT NULL DEFAULT 0,
                    user_agent TEXT
                );
                CREATE INDEX IF NOT EXISTS idx_auth_tokens_user
                    ON auth_tokens(user_id);
                CREATE INDEX IF NOT EXISTS idx_auth_tokens_hash
                    ON auth_tokens(token_hash);
                CREATE INDEX IF NOT EXISTS idx_auth_tokens_expires
                    ON auth_tokens(expires_at);

                CREATE TABLE IF NOT EXISTS auth_oidc_states (
                    state TEXT PRIMARY KEY,
                    created_at TEXT NOT NULL,
                    expires_at TEXT NOT NULL,
                    code_verifier TEXT
                );
            """)
            # Migration: add code_verifier (PKCE) to pre-existing auth_oidc_states.
            have = {row[1] for row in conn.execute("PRAGMA table_info(auth_oidc_states)")}
            if "code_verifier" not in have:
                conn.execute("ALTER TABLE auth_oidc_states ADD COLUMN code_verifier TEXT")
            conn.commit()
        finally:
            conn.close()

    @staticmethod
    def _to_user(row: sqlite3.Row | None) -> UserRecord | None:
        if row is None:
            return None
        return UserRecord(
            id = row["id"],
            display_name = row["display_name"],
            email = row["email"],
            avatar_url = row["avatar_url"],
            role = row["role"],
            created_at = row["created_at"],
            last_login_at = row["last_login_at"],
        )

    @staticmethod
    def _to_provider(row: sqlite3.Row | None) -> AuthProviderRecord | None:
        if row is None:
            return None
        return AuthProviderRecord(
            id = row["id"],
            user_id = row["user_id"],
            provider = row["provider"],
            provider_uid = row["provider_uid"],
            provider_data = row["provider_data"],
            created_at = row["created_at"],
        )

    @staticmethod
    def _to_token(row: sqlite3.Row | None) -> TokenRecord | None:
        if row is None:
            return None
        return TokenRecord(
            id = row["id"],
            user_id = row["user_id"],
            token_hash = row["token_hash"],
            issued_at = row["issued_at"],
            expires_at = row["expires_at"],
            last_seen_at = row["last_seen_at"],
            revoked = bool(row["revoked"]),
            user_agent = row["user_agent"],
        )

    async def has_any_users(self) -> bool:
        """Return True if at least one user exists. False triggers the setup screen."""
        def operation(conn: sqlite3.Connection) -> bool:
            row = conn.execute("SELECT 1 FROM auth_users LIMIT 1").fetchone()
            return row is not None
        return await self._read(operation)

    async def create_user(
        self,
        *,
        id: str,
        display_name: str,
        role: str,
        email: str | None = None,
        avatar_url: str | None = None,
    ) -> UserRecord:
        now = _now_iso()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """INSERT INTO auth_users (id, display_name, email, avatar_url, role, created_at)
                   VALUES (?, ?, ?, ?, ?, ?)""",
                (id, display_name, email, avatar_url, role, now),
            )

        await self._write(operation)
        return UserRecord(
            id = id,
            display_name = display_name,
            email = email,
            avatar_url = avatar_url,
            role = role,
            created_at = now,
        )

    async def get_user_by_id(self, user_id: str) -> UserRecord | None:
        def operation(conn: sqlite3.Connection) -> UserRecord | None:
            return self._to_user(
                conn.execute("SELECT * FROM auth_users WHERE id = ?", (user_id,)).fetchone()
            )
        return await self._read(operation)

    async def get_user_by_email(self, email: str) -> UserRecord | None:
        def operation(conn: sqlite3.Connection) -> UserRecord | None:
            return self._to_user(
                conn.execute(
                    "SELECT * FROM auth_users WHERE email = ?", (email.lower(),)
                ).fetchone()
            )
        return await self._read(operation)

    async def update_last_login(self, user_id: str) -> None:
        now = _now_iso()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "UPDATE auth_users SET last_login_at = ? WHERE id = ?", (now, user_id)
            )

        await self._write(operation)

    async def update_user_role(self, user_id: str, role: str) -> None:
        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "UPDATE auth_users SET role = ? WHERE id = ?", (role, user_id)
            )
        await self._write(operation)

    async def delete_user(self, user_id: str) -> bool:
        def operation(conn: sqlite3.Connection) -> bool:
            cursor = conn.execute("DELETE FROM auth_users WHERE id = ?", (user_id,))
            return cursor.rowcount > 0
        return await self._write(operation)

    async def update_user_profile(
        self,
        user_id: str,
        *,
        display_name: str | None = None,
        avatar_url: str | None = None,
    ) -> None:
        if display_name is None and avatar_url is None:
            return

        def operation(conn: sqlite3.Connection) -> None:
            if display_name is not None:
                conn.execute(
                    "UPDATE auth_users SET display_name = ? WHERE id = ?",
                    (display_name, user_id),
                )
            if avatar_url is not None:
                conn.execute(
                    "UPDATE auth_users SET avatar_url = ? WHERE id = ?",
                    (avatar_url, user_id),
                )

        await self._write(operation)

    async def list_users(self, limit: int = 100, offset: int = 0) -> list[UserRecord]:
        def operation(conn: sqlite3.Connection) -> list[UserRecord]:
            rows = conn.execute(
                "SELECT * FROM auth_users ORDER BY created_at ASC LIMIT ? OFFSET ?",
                (limit, offset),
            ).fetchall()
            return [u for row in rows if (u := self._to_user(row)) is not None]
        return await self._read(operation)

    async def count_users_by_role(self, role: str) -> int:
        def operation(conn: sqlite3.Connection) -> int:
            row = conn.execute(
                "SELECT COUNT(*) AS n FROM auth_users WHERE role = ?", (role,)
            ).fetchone()
            return row["n"] if row else 0
        return await self._read(operation)

    async def count_users(self) -> int:
        def operation(conn: sqlite3.Connection) -> int:
            row = conn.execute("SELECT COUNT(*) AS n FROM auth_users").fetchone()
            return row["n"] if row else 0
        return await self._read(operation)

    async def create_auth_provider(
        self,
        *,
        id: str,
        user_id: str,
        provider: str,
        provider_uid: str,
        provider_data: str | None = None,
    ) -> AuthProviderRecord:
        now = _now_iso()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """INSERT INTO auth_providers
                   (id, user_id, provider, provider_uid, provider_data, created_at)
                   VALUES (?, ?, ?, ?, ?, ?)""",
                (id, user_id, provider, provider_uid, provider_data, now),
            )

        await self._write(operation)
        return AuthProviderRecord(
            id = id,
            user_id = user_id,
            provider = provider,
            provider_uid = provider_uid,
            provider_data = provider_data,
            created_at = now,
        )

    async def get_auth_provider(self, provider: str, provider_uid: str) -> AuthProviderRecord | None:
        def operation(conn: sqlite3.Connection) -> AuthProviderRecord | None:
            return self._to_provider(
                conn.execute(
                    "SELECT * FROM auth_providers WHERE provider = ? AND provider_uid = ?",
                    (provider, provider_uid),
                ).fetchone()
            )
        return await self._read(operation)

    async def list_providers_for_user(self, user_id: str) -> list[AuthProviderRecord]:
        def operation(conn: sqlite3.Connection) -> list[AuthProviderRecord]:
            rows = conn.execute(
                "SELECT * FROM auth_providers WHERE user_id = ? ORDER BY created_at ASC",
                (user_id,),
            ).fetchall()
            return [p for row in rows if (p := self._to_provider(row)) is not None]
        return await self._read(operation)

    async def list_provider_names_for_users(self, user_ids: list[str]) -> dict[str, list[str]]:
        if not user_ids:
            return {}

        def operation(conn: sqlite3.Connection) -> dict[str, list[str]]:
            placeholders = ",".join("?" for _ in user_ids)
            rows = conn.execute(
                f"SELECT DISTINCT user_id, provider FROM auth_providers WHERE user_id IN ({placeholders}) "
                "ORDER BY created_at ASC",
                user_ids,
            ).fetchall()
            result: dict[str, list[str]] = {}
            for row in rows:
                result.setdefault(row["user_id"], []).append(row["provider"])
            return result
        return await self._read(operation)

    async def delete_auth_provider(self, provider_id: str) -> bool:
        def operation(conn: sqlite3.Connection) -> bool:
            cursor = conn.execute(
                "DELETE FROM auth_providers WHERE id = ?", (provider_id,)
            )
            return cursor.rowcount > 0
        return await self._write(operation)

    async def update_provider_data(self, provider_id: str, provider_data: str) -> None:
        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "UPDATE auth_providers SET provider_data = ? WHERE id = ?",
                (provider_data, provider_id),
            )
        await self._write(operation)

    def issue_token(self) -> tuple[str, str]:
        """Generate a new raw token and its hash.

        Returns (raw_token, token_hash).
        Only the hash is stored, the raw token is returned once and never stored server-side.
        """
        raw = urlsafe_b64encode(os.urandom(TOKEN_BYTES)).decode()
        hashed = _hash_token(raw)
        return raw, hashed

    async def store_token(
        self,
        *,
        id: str,
        user_id: str,
        token_hash: str,
        user_agent: str | None = None,
    ) -> TokenRecord:
        now = _now_iso()
        expiry = _expiry_iso()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """INSERT INTO auth_tokens
                   (id, user_id, token_hash, issued_at, expires_at, last_seen_at, revoked, user_agent)
                   VALUES (?, ?, ?, ?, ?, ?, 0, ?)""",
                (id, user_id, token_hash, now, expiry, now, user_agent),
            )

        await self._write(operation)
        return TokenRecord(
            id = id,
            user_id = user_id,
            token_hash = token_hash,
            issued_at = now,
            expires_at = expiry,
            last_seen_at = now,
            revoked = False,
            user_agent = user_agent,
        )

    async def verify_token(self, raw_token: str) -> TokenRecord | None:
        candidate_hash = _hash_token(raw_token)
        now = _now_iso()

        def operation(conn: sqlite3.Connection) -> TokenRecord | None:
            row = conn.execute(
                "SELECT * FROM auth_tokens WHERE token_hash = ? AND revoked = 0 AND expires_at > ?",
                (candidate_hash, now),
            ).fetchone()
            if row is None:
                return None
            stored_hash: str = row["token_hash"]
            if not hmac.compare_digest(stored_hash.encode(), candidate_hash.encode()):
                return None
            return self._to_token(row)

        return await self._read(operation)

    async def touch_token(self, token_id: str) -> None:
        now = _now_iso()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "UPDATE auth_tokens SET last_seen_at = ? WHERE id = ?", (now, token_id)
            )

        await self._write(operation)

    async def revoke_token(self, token_id: str) -> None:
        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "UPDATE auth_tokens SET revoked = 1 WHERE id = ?", (token_id,)
            )
        await self._write(operation)

    async def revoke_all_tokens_for_user(self, user_id: str, *, except_token_id: str | None = None) -> None:
        def operation(conn: sqlite3.Connection) -> None:
            if except_token_id:
                conn.execute(
                    "UPDATE auth_tokens SET revoked = 1 WHERE user_id = ? AND id != ?",
                    (user_id, except_token_id),
                )
            else:
                conn.execute(
                    "UPDATE auth_tokens SET revoked = 1 WHERE user_id = ?", (user_id,)
                )
        await self._write(operation)

    async def list_tokens_for_user(self, user_id: str) -> list[TokenRecord]:
        now = _now_iso()

        def operation(conn: sqlite3.Connection) -> list[TokenRecord]:
            rows = conn.execute(
                """SELECT * FROM auth_tokens
                   WHERE user_id = ? AND revoked = 0 AND expires_at > ?
                   ORDER BY last_seen_at DESC""",
                (user_id, now),
            ).fetchall()
            return [t for row in rows if (t := self._to_token(row)) is not None]

        return await self._read(operation)

    async def cleanup_expired_tokens(self) -> int:
        now = _now_iso()

        def operation(conn: sqlite3.Connection) -> int:
            cursor = conn.execute(
                "DELETE FROM auth_tokens WHERE expires_at < ?", (now,)
            )
            return cursor.rowcount

        count = await self._write(operation)
        if count:
            logger.info(f"Cleaned up {count} expired auth token(s)")
        return count

    async def store_oidc_state(
        self, state: str, ttl_seconds: int = 600, code_verifier: str | None = None
    ) -> None:
        now = _now_iso()
        expiry = (datetime.now(timezone.utc) + timedelta(seconds = ttl_seconds)).isoformat()

        def operation(conn: sqlite3.Connection) -> None:
            # Also prune stale states opportunistically
            conn.execute("DELETE FROM auth_oidc_states WHERE expires_at < ?", (now,))
            conn.execute(
                "INSERT INTO auth_oidc_states (state, created_at, expires_at, code_verifier) VALUES (?, ?, ?, ?)",
                (state, now, expiry, code_verifier),
            )

        await self._write(operation)

    async def consume_oidc_state(self, state: str) -> tuple[bool, str | None]:
        now = _now_iso()

        def operation(conn: sqlite3.Connection) -> tuple[bool, str | None]:
            row = conn.execute(
                "SELECT code_verifier FROM auth_oidc_states WHERE state = ? AND expires_at > ?",
                (state, now),
            ).fetchone()
            if row is None:
                return False, None
            conn.execute("DELETE FROM auth_oidc_states WHERE state = ?", (state,))
            return True, row["code_verifier"]

        return await self._write(operation)


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def _expiry_iso() -> str:
    return (datetime.now(timezone.utc) + timedelta(days = TOKEN_LIFETIME_DAYS)).isoformat()


def _hash_token(raw_token: str) -> str:
    return hashlib.sha256(raw_token.encode()).hexdigest()
