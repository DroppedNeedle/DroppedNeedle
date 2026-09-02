"""Password-login policy.

``ALLOW_PASSWORD_LOGIN`` (default ``true``) is a deployment-level switch, not an
in-app preference. It lives in the environment deliberately: an operator who
disables local credentials and then loses their identity provider needs a
recovery path that does not itself require signing in. Flipping the variable
back to ``true`` and restarting the container is that path.

Disabling it closes every route that accepts or writes a local password:

* ``POST /api/v1/auth/login``
* ``POST /api/v1/auth/password-recovery/reset``
* ``POST /api/v1/profile/password``
* ``POST /api/v1/profile/set-password``

``POST /api/v1/auth/setup`` is deliberately left open. It already refuses with
409 once a user exists, so it is a one-shot bootstrap rather than a standing
credential surface, and without it a fresh instance configured for SSO-only has
no way to create its first administrator (OIDC first-login provisions the
``User`` role, not ``Admin``).

Third-party app-passwords (Settings > Connect Apps) are a separate credential
system used by OpenSubsonic and Jellyfin clients, which cannot perform an OIDC
flow. They are intentionally not governed by this switch.
"""

from __future__ import annotations

from fastapi import HTTPException, status

from core.config import get_settings


def password_login_enabled() -> bool:
    """Whether local username/password authentication is permitted."""
    return get_settings().allow_password_login


def require_password_login_enabled() -> None:
    """FastAPI dependency: 403 when local password auth is disabled.

    Used as a route-level guard so the rejection happens before the request body
    is bound and before any credential reaches the auth service.
    """
    if not password_login_enabled():
        raise HTTPException(
            status_code=status.HTTP_403_FORBIDDEN,
            detail=(
                "Password authentication is disabled on this instance. "
                "Sign in through your identity provider."
            ),
        )
