"""PKCE (RFC 7636) coverage for the OIDC login flow.

Written against the spec, not the implementation: section 4.1 constrains the
code verifier, section 4.2 defines the S256 challenge, and the verifier must
survive the round trip from authorize-redirect to callback via AuthStore.
"""

import base64
import hashlib
import string

import pytest

from infrastructure.persistence.auth_store import AuthStore
from services.oidc_user_auth_service import _pkce_pair

_UNRESERVED = set(string.ascii_letters + string.digits + "-._~")


def test_verifier_meets_rfc7636_section_4_1():
    code_verifier, _ = _pkce_pair()
    assert 43 <= len(code_verifier) <= 128
    assert set(code_verifier) <= _UNRESERVED


def test_challenge_is_unpadded_s256_of_verifier():
    code_verifier, code_challenge = _pkce_pair()
    digest = hashlib.sha256(code_verifier.encode("ascii")).digest()
    assert code_challenge == base64.urlsafe_b64encode(digest).decode().rstrip("=")
    assert not code_challenge.endswith("=")


def test_every_pair_is_fresh():
    verifiers = {_pkce_pair()[0] for _ in range(8)}
    assert len(verifiers) == 8


@pytest.mark.asyncio
async def test_verifier_survives_state_round_trip(tmp_path):
    store = AuthStore(tmp_path / "pkce.db")
    await store.store_oidc_state("st", code_verifier="cv-123")

    assert await store.consume_oidc_state("st") == (True, "cv-123")


@pytest.mark.asyncio
async def test_state_is_single_use(tmp_path):
    store = AuthStore(tmp_path / "pkce.db")
    await store.store_oidc_state("st", code_verifier="cv-123")

    await store.consume_oidc_state("st")
    assert await store.consume_oidc_state("st") == (False, None)


@pytest.mark.asyncio
async def test_pre_pkce_state_rows_still_validate(tmp_path):
    # A state stored without a verifier (pre-PKCE deployment) must stay loginable.
    store = AuthStore(tmp_path / "pkce.db")
    await store.store_oidc_state("st")

    assert await store.consume_oidc_state("st") == (True, None)


@pytest.mark.asyncio
async def test_expired_state_is_rejected(tmp_path):
    store = AuthStore(tmp_path / "pkce.db")
    await store.store_oidc_state("st", ttl_seconds=-1, code_verifier="cv-123")

    assert await store.consume_oidc_state("st") == (False, None)
