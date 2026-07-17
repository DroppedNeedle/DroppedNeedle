"""Auth persistence store: users, auth_providers, tokens tables."""

from __future__ import annotations

import asyncio, hashlib, hmac, logging, os, re, sqlite3, threading
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
    username: str | None = None          # lowercased, unique, used for login lookup
    username_display: str | None = None  # preferred casing; NULL -> fall back to username


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

                CREATE TABLE IF NOT EXISTS auth_password_recovery_codes (
                    user_id TEXT PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
                    code_hash TEXT NOT NULL UNIQUE,
                    created_at TEXT NOT NULL,
                    expires_at TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_auth_password_recovery_expires
                    ON auth_password_recovery_codes(expires_at);

                CREATE TABLE IF NOT EXISTS auth_oidc_states (
                    state TEXT PRIMARY KEY,
                    created_at TEXT NOT NULL,
                    expires_at TEXT NOT NULL,
                    code_verifier TEXT -- PKCE; NULL on rows stored before it existed
                );

                CREATE TABLE IF NOT EXISTS spotify_oauth_states (
                    state TEXT PRIMARY KEY,
                    user_id TEXT NOT NULL,
                    expires_at TEXT NOT NULL
                );
            """)
            # PKCE column ratchet: pre-PKCE databases lack it; additive, idempotent.
            try:
                conn.execute("ALTER TABLE auth_oidc_states ADD COLUMN code_verifier TEXT")
            except sqlite3.OperationalError:
                pass  # duplicate column - already present
            # Username login (D3): additive, idempotent. `username` is the lowercased
            # login identifier; `username_display` preserves preferred casing. The
            # partial unique index lets pre-backfill NULL rows coexist.
            for column in ("username", "username_display"):
                try:
                    conn.execute(f"ALTER TABLE auth_users ADD COLUMN {column} TEXT")
                except sqlite3.OperationalError:
                    pass  # duplicate column - already present
            conn.execute(
                "CREATE UNIQUE INDEX IF NOT EXISTS idx_auth_users_username "
                "ON auth_users(username) WHERE username IS NOT NULL"
            )
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
            username = row["username"],
            username_display = row["username_display"],
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
        username: str | None = None,
        username_display: str | None = None,
    ) -> UserRecord:
        now = _now_iso()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """INSERT INTO auth_users
                   (id, display_name, email, avatar_url, role, created_at, username, username_display)
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?)""",
                (id, display_name, email, avatar_url, role, now, username, username_display),
            )

        await self._write(operation)
        return UserRecord(
            id = id,
            display_name = display_name,
            email = email,
            avatar_url = avatar_url,
            role = role,
            created_at = now,
            username = username,
            username_display = username_display,
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

    async def get_user_by_username(self, username: str) -> UserRecord | None:
        """Look up a user by their lowercased username (D3 login lookup).

        The service lowercases input before calling, so the stored and queried
        values match.
        """
        def operation(conn: sqlite3.Connection) -> UserRecord | None:
            return self._to_user(
                conn.execute(
                    "SELECT * FROM auth_users WHERE username = ?", (username,)
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
            # playlists.user_id is an ALTER-added column and cannot carry ON DELETE
            # CASCADE, so delete the user's playlists explicitly first (their
            # playlist_tracks cascade via the playlist_id FK). Same connection, FK
            # pragma on. Owner reassignment is NOT offered (AMU-1 / M4).
            conn.execute("DELETE FROM playlists WHERE user_id = ?", (user_id,))
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

    async def update_username(
        self,
        user_id: str,
        username: str,
        username_display: str,
        *,
        local_provider_id: str | None = None,
    ) -> None:
        """Rename a user's username, atomically syncing their local provider_uid (D8, M3).

        Both UPDATEs run in one transaction so a provider_uid collision rolls the
        username change back too - without the sync, login_local(new) would fail
        after a rename because the local provider's provider_uid still held the old
        value. Relies on the unique indexes to raise sqlite3.IntegrityError on a
        collision (the service maps that to a domain error).
        """
        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "UPDATE auth_users SET username = ?, username_display = ? WHERE id = ?",
                (username, username_display, user_id),
            )
            if local_provider_id is not None:
                conn.execute(
                    "UPDATE auth_providers SET provider_uid = ? WHERE id = ?",
                    (username, local_provider_id),
                )

        await self._write(operation)

    async def update_email(self, user_id: str, email: str | None) -> None:
        """Set (or clear, when None) a user's email (D8). UNIQUE - may raise IntegrityError."""
        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "UPDATE auth_users SET email = ? WHERE id = ?", (email, user_id)
            )

        await self._write(operation)

    async def get_first_admin(self) -> UserRecord | None:
        """Earliest-created admin (D7/D9/D10 first-admin selection for backfills)."""
        def operation(conn: sqlite3.Connection) -> UserRecord | None:
            return self._to_user(
                conn.execute(
                    "SELECT * FROM auth_users WHERE role = 'admin' "
                    "ORDER BY created_at ASC LIMIT 1"
                ).fetchone()
            )
        return await self._read(operation)

    async def backfill_usernames(self) -> None:
        """Assign a derived unique username to every user missing one (D3).

        Idempotent: rows that already have a username are skipped, so re-running
        at boot is a no-op. MUST run before migrate_local_provider_to_username().
        """
        def _load(conn: sqlite3.Connection) -> list[tuple[str, str | None, str]]:
            rows = conn.execute(
                "SELECT id, email, display_name FROM auth_users WHERE username IS NULL"
            ).fetchall()
            return [(r["id"], r["email"], r["display_name"]) for r in rows]

        for user_id, email, display_name in await self._read(_load):
            await self._assign_unique_username(user_id, email = email, display_name = display_name)

    async def _assign_unique_username(
        self, user_id: str, *, email: str | None, display_name: str
    ) -> None:
        """Allocate a unique username from the derived base and persist it.

        Retries on a unique-index collision (the TOCTOU race between the de-dup
        check and the UPDATE) so a transient clash never aborts the backfill.
        """
        for _ in range(50):
            username, display = await _derive_username(self, email = email, display_name = display_name)

            def operation(conn: sqlite3.Connection, u = username, d = display) -> None:
                conn.execute(
                    "UPDATE auth_users SET username = ?, username_display = ? WHERE id = ?",
                    (u, d, user_id),
                )

            try:
                await self._write(operation)
                return
            except sqlite3.IntegrityError:
                continue  # lost the race; re-derive picks the next free suffix
        logger.warning("Could not allocate a unique username for %s after retries", user_id[:8])

    async def migrate_local_provider_to_username(self) -> None:
        """Point each local provider's provider_uid at the user's username (D3).

        MUST run after backfill_usernames(). Idempotent: the UPDATE is convergent
        (re-running rewrites the same value); non-local providers are untouched and
        rows whose user has no username are skipped (never writes a NULL uid).
        """
        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "UPDATE auth_providers "
                "SET provider_uid = (SELECT username FROM auth_users WHERE id = auth_providers.user_id) "
                "WHERE provider = 'local' "
                "  AND (SELECT username FROM auth_users WHERE id = auth_providers.user_id) IS NOT NULL"
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

    async def get_users_by_ids(self, user_ids: list[str]) -> dict[str, UserRecord]:
        """Bulk id -> UserRecord lookup for owner enrichment. One IN-clause query
        (no LIMIT, unlike list_users) so callers never silently truncate."""
        if not user_ids:
            return {}

        def operation(conn: sqlite3.Connection) -> dict[str, UserRecord]:
            placeholders = ",".join("?" for _ in user_ids)
            rows = conn.execute(
                f"SELECT * FROM auth_users WHERE id IN ({placeholders})",
                user_ids,
            ).fetchall()
            result: dict[str, UserRecord] = {}
            for row in rows:
                user = self._to_user(row)
                if user is not None:
                    result[user.id] = user
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

    async def change_local_password(
        self,
        *,
        provider_id: str,
        user_id: str,
        expected_provider_data: str,
        provider_data: str,
    ) -> bool:
        """Update an unchanged password and invalidate recovery in one transaction."""

        def operation(conn: sqlite3.Connection) -> bool:
            cursor = conn.execute(
                """UPDATE auth_providers SET provider_data = ?
                   WHERE id = ? AND user_id = ? AND provider = 'local'
                     AND provider_data = ?""",
                (provider_data, provider_id, user_id, expected_provider_data),
            )
            if cursor.rowcount == 0:
                return False
            conn.execute(
                "DELETE FROM auth_password_recovery_codes WHERE user_id = ?",
                (user_id,),
            )
            return True

        return await self._write(operation)

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

    async def store_password_recovery_code(
        self,
        *,
        user_id: str,
        code_hash: str,
        expires_at: str,
    ) -> None:
        """Replace a user's recovery code so only the newest code can be used."""
        now = _now_iso()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "DELETE FROM auth_password_recovery_codes WHERE expires_at <= ?",
                (now,),
            )
            conn.execute(
                """INSERT INTO auth_password_recovery_codes
                   (user_id, code_hash, created_at, expires_at)
                   VALUES (?, ?, ?, ?)
                   ON CONFLICT(user_id) DO UPDATE SET
                       code_hash = excluded.code_hash,
                       created_at = excluded.created_at,
                       expires_at = excluded.expires_at""",
                (user_id, code_hash, now, expires_at),
            )

        await self._write(operation)

    async def reset_password_with_recovery_code(
        self,
        *,
        username: str,
        code_hash: str,
        provider_data: str,
    ) -> bool:
        """Consume a valid code, change the password, and revoke web sessions atomically."""
        now = _now_iso()

        def operation(conn: sqlite3.Connection) -> bool:
            conn.execute(
                "DELETE FROM auth_password_recovery_codes WHERE expires_at <= ?",
                (now,),
            )
            row = conn.execute(
                """SELECT recovery.user_id, provider.id AS provider_id
                   FROM auth_password_recovery_codes AS recovery
                   JOIN auth_users AS user ON user.id = recovery.user_id
                   JOIN auth_providers AS provider
                     ON provider.user_id = recovery.user_id AND provider.provider = 'local'
                   WHERE user.username = ? AND recovery.code_hash = ?
                     AND recovery.expires_at > ?""",
                (username, code_hash, now),
            ).fetchone()
            if row is None:
                return False

            conn.execute(
                "UPDATE auth_providers SET provider_data = ? WHERE id = ?",
                (provider_data, row["provider_id"]),
            )
            conn.execute(
                "UPDATE auth_tokens SET revoked = 1 WHERE user_id = ?",
                (row["user_id"],),
            )
            conn.execute(
                "DELETE FROM auth_password_recovery_codes WHERE user_id = ?",
                (row["user_id"],),
            )
            return True

        return await self._write(operation)

    async def cleanup_expired_password_recovery_codes(self) -> int:
        now = _now_iso()

        def operation(conn: sqlite3.Connection) -> int:
            cursor = conn.execute(
                "DELETE FROM auth_password_recovery_codes WHERE expires_at <= ?",
                (now,),
            )
            return cursor.rowcount

        return await self._write(operation)

    async def store_oidc_state(self, state: str, ttl_seconds: int = 600, code_verifier: str | None = None) -> None:
        now = _now_iso()
        expiry = (datetime.now(timezone.utc) + timedelta(seconds = ttl_seconds)).isoformat()

        def operation(conn: sqlite3.Connection) -> None:
            # Piggyback a sweep of already-expired states on every insert.
            conn.execute("DELETE FROM auth_oidc_states WHERE expires_at < ?", (now,))
            conn.execute(
                "INSERT INTO auth_oidc_states (state, code_verifier, created_at, expires_at)"
                " VALUES (?, ?, ?, ?)",
                (state, code_verifier, now, expiry),
            )

        await self._write(operation)

    async def consume_oidc_state(self, state: str) -> tuple[bool, str | None]:  # (valid, code_verifier)
        now = _now_iso()

        def operation(conn: sqlite3.Connection) -> tuple[bool, str | None]:
            # Look up and delete inside one _write closure, so the state is
            # single-use: a replayed callback finds nothing.
            found = conn.execute(
                "SELECT code_verifier FROM auth_oidc_states"
                " WHERE state = ? AND expires_at > ?",
                (state, now),
            ).fetchone()
            if found is None:
                return False, None
            conn.execute("DELETE FROM auth_oidc_states WHERE state = ?", (state,))
            return True, found["code_verifier"]

        return await self._write(operation)

    async def store_spotify_state(self, state: str, user_id: str, ttl_seconds: int = 600) -> None:
        now = _now_iso()
        expiry = (datetime.now(timezone.utc) + timedelta(seconds=ttl_seconds)).isoformat()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute("DELETE FROM spotify_oauth_states WHERE expires_at < ?", (now,))
            conn.execute(
                "INSERT INTO spotify_oauth_states (state, user_id, expires_at) VALUES (?, ?, ?)",
                (state, user_id, expiry),
            )

        await self._write(operation)

    async def consume_spotify_state(self, state: str) -> str | None:
        now = _now_iso()

        def operation(conn: sqlite3.Connection) -> str | None:
            row = conn.execute(
                "SELECT user_id FROM spotify_oauth_states WHERE state = ? AND expires_at > ?",
                (state, now),
            ).fetchone()
            if row is None:
                return None
            conn.execute("DELETE FROM spotify_oauth_states WHERE state = ?", (state,))
            return row["user_id"]

        return await self._write(operation)


_USERNAME_SUB = re.compile(r"[^a-zA-Z0-9._-]+")


def _slugify(raw: str) -> str:
    """Reduce a string to the username charset [a-zA-Z0-9._-], preserving case.

    Disallowed runs collapse to '-'; repeated '-' collapse; leading/trailing
    separators are trimmed. Returns "" when nothing usable remains.
    """
    s = _USERNAME_SUB.sub("-", (raw or "").strip())
    s = re.sub(r"-{2,}", "-", s).strip("-._")
    return s


def _username_base(*, email: str | None = None, display_name: str | None = None) -> str:
    """Pick a base username candidate: email local-part, else display_name, else 'user'.

    Case is preserved here; the caller lowercases for storage (D3).
    """
    local_part = email.split("@", 1)[0] if email and "@" in email else ""
    return _slugify(local_part) or _slugify(display_name or "") or "user"


async def _derive_username(
    store, *, email: str | None = None, display_name: str | None = None
) -> tuple[str, str]:
    """Derive a unique (username_lowercased, username_display) against `store`.

    De-dups with a numeric suffix (jane, jane-2, jane-3, …). Reused by the
    username backfill and the SSO auto-create flows (D3). `store` must expose an
    async `get_user_by_username`.
    """
    base = _username_base(email = email, display_name = display_name)
    n = 1
    while True:
        username = base.lower() if n == 1 else f"{base.lower()}-{n}"
        display = base if n == 1 else f"{base}-{n}"
        if await store.get_user_by_username(username) is None:
            return username, display
        n += 1


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def _expiry_iso() -> str:
    return (datetime.now(timezone.utc) + timedelta(days = TOKEN_LIFETIME_DAYS)).isoformat()


def _hash_token(raw_token: str) -> str:
    return hashlib.sha256(raw_token.encode()).hexdigest()
