"""Guardrail 2: DroppedNeedle never joins the Soulseek/P2P network.

The engine drives a *user-supplied* slskd instance over slskd's local HTTP API
and imports what lands. It has no Soulseek protocol code, opens no peer
connections, and never distributes. The operator runs slskd and owns its shared
folders.

The README's "Legality boundary" section states this, and states that it is
architectural rather than a UI promise. That claim holds only for as long as the
code stays this way, so these tests hold the line.

Adding raw sockets to ``repositories/slskd/`` would turn a client for a service
the user runs into a participant in the network. That is a different piece of
software with a different legal posture, and it should not be possible to ship
it by accident.
"""

import ast
import re
from pathlib import Path

_BACKEND = Path(__file__).resolve().parents[2]
_REPO = _BACKEND.parent
_SLSKD = _BACKEND / "repositories" / "slskd"

# Python implementations of the Soulseek wire protocol. ``slskd`` itself is not
# on this list: it is the daemon the *operator* runs, and we only ever hold an
# HTTP client for it. The negative lookahead keeps `slskd` from matching `slsk`.
_PROTOCOL_LIBS = re.compile(
    r"\b(aioslsk|pyslsk|soulseek|museek|nicotine|slsk(?!d))\b",
    re.IGNORECASE,
)

# Opening our own connection, rather than issuing an HTTP request through the
# injected client, would make us a peer rather than a client of the operator's slskd.
_RAW_TRANSPORT_MODULES = {"socket"}
_RAW_TRANSPORT_ATTRS = {
    "socket",
    "open_connection",
    "create_connection",
    "create_server",
}

_SKIP_DIRS = {".venv", "__pycache__"}


def _backend_modules() -> list[Path]:
    return [
        path
        for path in _BACKEND.rglob("*.py")
        if not any(part in _SKIP_DIRS for part in path.parts)
        and "tests" not in path.parts
    ]


def _parse(path: Path) -> ast.Module:
    return ast.parse(path.read_text(encoding="utf-8", errors="ignore"), filename=str(path))


def _import_roots(node: ast.AST) -> list[str]:
    """The top-level package of each name an import statement binds."""
    if isinstance(node, ast.Import):
        return [alias.name.split(".", 1)[0] for alias in node.names]
    if isinstance(node, ast.ImportFrom):
        return [(node.module or "").split(".", 1)[0]]
    return []


def test_no_soulseek_protocol_library_is_a_dependency():
    for name in ("requirements.txt", "requirements-dev.txt"):
        requirements = _BACKEND / name
        for line in requirements.read_text(encoding="utf-8").splitlines():
            package = line.split("#", 1)[0].strip()
            if not package:
                continue
            assert not _PROTOCOL_LIBS.search(package), (
                f"Guardrail 2 broken: {name} pulls in a Soulseek protocol library "
                f"({package!r}). DroppedNeedle drives slskd over HTTP; it does not "
                "speak Soulseek."
            )


def test_no_module_imports_a_soulseek_protocol_library():
    """Parsed, not grepped: prose in a docstring can begin with the word "import"."""
    offenders: list[str] = []

    for path in _backend_modules():
        for node in ast.walk(_parse(path)):
            for name in _import_roots(node):
                if _PROTOCOL_LIBS.search(name):
                    offenders.append(f"{path.relative_to(_REPO)}:{node.lineno}: {name}")

    assert offenders == [], "Guardrail 2 broken: Soulseek protocol import\n  " + "\n  ".join(
        offenders
    )


def test_slskd_repository_opens_no_raw_connections():
    """Every byte leaving ``repositories/slskd`` goes over the injected HTTP client.

    Parsed rather than grepped, so a docstring naming a primitive we promise not to
    call does not read as a call to it.
    """
    offenders: list[str] = []

    for path in sorted(_SLSKD.glob("*.py")):
        for node in ast.walk(_parse(path)):
            for name in _import_roots(node):
                if name in _RAW_TRANSPORT_MODULES:
                    offenders.append(f"{path.relative_to(_REPO)}:{node.lineno}: import {name}")

            if isinstance(node, ast.Attribute) and (
                node.attr in _RAW_TRANSPORT_ATTRS or node.attr.startswith("sock_")
            ):
                offenders.append(f"{path.relative_to(_REPO)}:{node.lineno}: .{node.attr}")

    assert offenders == [], (
        "Guardrail 2 broken: repositories/slskd opened its own connection. It must "
        "only ever issue HTTP requests to a user-supplied slskd.\n  " + "\n  ".join(offenders)
    )


def test_slskd_repository_still_speaks_http():
    """The positive half of the invariant.

    Without this, swapping the HTTP client for sockets would satisfy the negative
    tests above right up until someone renamed a primitive.
    """
    roots = {
        name
        for node in ast.walk(_parse(_SLSKD / "slskd_client.py"))
        for name in _import_roots(node)
    }
    assert "httpx" in roots, "the slskd client no longer holds an HTTP client"
