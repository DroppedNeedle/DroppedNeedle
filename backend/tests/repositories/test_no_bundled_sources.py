"""Guardrail 1: DroppedNeedle ships no sources.

The user supplies every source the app can reach - their own slskd instance, or
their own Newznab indexers and their own SABnzbd. Nothing is preset, bundled,
recommended, or curated in-tree.

This is the load-bearing distinction between an automation tool and an index, and
the README's "Legality boundary" section asserts it in prose. It held until now
because we remembered it. This test makes CI remember instead.

Scope: shipped source only. ``backend/tests/`` is exempt because the Newznab
mock in ``tests/mocks/`` is the executable record of live-verified indexer
behaviour and is *named after* the real services it was captured from - that is
the house pattern for stateful integrations, not a bundled source.
"""

from pathlib import Path

import pytest

from api.v1.schemas.settings import NewznabIndexerSettings

_BACKEND = Path(__file__).resolve().parents[2]
_REPO = _BACKEND.parent

# Domains of public indexers, trackers, and the Soulseek servers themselves.
#
# We match *domains*, not bare service names, for two reasons. A shipped source
# is a reachable address; a docstring naming the indexers a parser was verified
# against (``routes/indexers.py``) is the opposite - it is the house rule on
# recording what you probed live. And bare names collide: "abnzb" is a substring
# of "sabnzbd".
_FORBIDDEN_DOMAINS = (
    # Usenet indexers
    "nzbgeek.info",
    "nzbplanet.net",
    "drunkenslug.com",
    "nzbfinder.ws",
    "omgwtfnzbs.org",
    "dognzb.cr",
    "usenet-crawler.com",
    "tabula-rasa.pw",
    "ninjacentral.co.za",
    "althub.co.za",
    "nzb.su",
    # Torrent trackers
    "thepiratebay.org",
    "rarbg.to",
    "1337x.to",
    "torrentleech.org",
    "iptorrents.com",
    "redacted.ch",
    "orpheus.network",
    "nyaa.si",
    # Soulseek network servers (we speak to slskd over HTTP, never to these)
    "slsknet.org",
)

# A subdomain (``api.nzbgeek.info``) carries its parent domain as a substring, so
# plain containment catches those too.

_SKIP_DIRS = {".venv", "__pycache__", "node_modules", ".svelte-kit", "build", "dist"}


def _shipped_sources() -> list[Path]:
    """Every source file that ends up in a release, excluding tests."""
    files: list[Path] = []

    for path in _BACKEND.rglob("*.py"):
        if any(part in _SKIP_DIRS for part in path.parts):
            continue
        if "tests" in path.parts:
            continue
        files.append(path)

    frontend_src = _REPO / "frontend" / "src"
    for pattern in ("*.ts", "*.svelte"):
        for path in frontend_src.rglob(pattern):
            if any(part in _SKIP_DIRS or part == "__tests__" for part in path.parts):
                continue
            # Covers `*.svelte.spec.ts` too.
            if path.name.endswith(".spec.ts"):
                continue
            files.append(path)

    return files


def test_no_indexer_or_tracker_domain_in_shipped_source():
    """No source the user did not configure is reachable from a stock install."""
    offenders: list[str] = []

    for path in _shipped_sources():
        haystack = path.read_text(encoding="utf-8", errors="ignore").lower()
        for domain in _FORBIDDEN_DOMAINS:
            if domain in haystack:
                offenders.append(f"{path.relative_to(_REPO)}: {domain!r}")

    assert offenders == [], (
        "Guardrail 1 broken: shipped source carries the address of a source the user "
        "did not configure. DroppedNeedle must ship no indexers, trackers, or sources.\n  "
        + "\n  ".join(offenders)
    )


def test_newznab_indexer_settings_ship_empty():
    """A freshly constructed indexer carries no preset endpoint or credential.

    A default url or api_key here would let a stock install reach a source nobody
    chose, whatever the README says.
    """
    indexer = NewznabIndexerSettings()

    assert indexer.id == ""
    assert indexer.name == ""
    assert indexer.url == ""
    assert indexer.api_key == ""


def test_no_indexers_are_preconfigured_in_the_example_config():
    """The legacy example config predates the current runtime shape, but it is
    still shipped, still read by humans, and must not seed a source."""
    example = _REPO / "config" / "config.example.json"
    if not example.exists():
        pytest.skip("config.example.json has been removed")

    body = example.read_text(encoding="utf-8", errors="ignore").lower()
    for domain in _FORBIDDEN_DOMAINS:
        assert domain not in body, f"config.example.json seeds a source: {domain!r}"
