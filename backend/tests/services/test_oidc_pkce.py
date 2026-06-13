"""Tests for OIDC PKCE: challenge derivation and verifier persistence."""

import base64
import hashlib

import pytest

from infrastructure.persistence.auth_store import AuthStore
from services.oidc_user_auth_service import _make_pkce


def test_make_pkce_format_and_challenge():
    verifier, challenge = _make_pkce()

    # RFC 7636: verifier is 43-128 chars from the unreserved set.
    assert 43 <= len(verifier) <= 128
    assert all(c.isalnum() or c in "-._~" for c in verifier)

    # challenge == base64url(SHA256(verifier)), unpadded.
    expected = (
        base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest())
        .rstrip(b"=")
        .decode()
    )
    assert challenge == expected
    assert "=" not in challenge


def test_make_pkce_is_random():
    assert _make_pkce()[0] != _make_pkce()[0]


@pytest.mark.asyncio
async def test_oidc_state_roundtrips_verifier(tmp_path):
    store = AuthStore(tmp_path / "auth.db")
    await store.store_oidc_state("state-1", code_verifier="verifier-abc")

    valid, verifier = await store.consume_oidc_state("state-1")
    assert valid is True
    assert verifier == "verifier-abc"

    # Single-use: a second consume fails and yields no verifier.
    valid2, verifier2 = await store.consume_oidc_state("state-1")
    assert valid2 is False
    assert verifier2 is None


@pytest.mark.asyncio
async def test_oidc_state_without_verifier(tmp_path):
    store = AuthStore(tmp_path / "auth.db")
    await store.store_oidc_state("state-2")

    valid, verifier = await store.consume_oidc_state("state-2")
    assert valid is True
    assert verifier is None
