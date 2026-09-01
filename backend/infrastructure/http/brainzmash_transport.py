"""Pinned, server-owned HTTP transport for the BrainzMash source."""

from __future__ import annotations

import asyncio
import ipaddress
import socket
from typing import Any
from urllib.parse import urlsplit

import httpx

BRAINZMASH_ENDPOINT = "https://api.brainzmash.cc/ws/2"
BRAINZMASH_HOST = "api.brainzmash.cc"
BRAINZMASH_USER_AGENT = "DroppedNeedleApp"
_BRAINZMASH_ENTITY_PATHS = frozenset(
    {"artist", "release-group", "release", "recording", "isrc", "url"}
)


class _PinnedBrainzMashNetworkBackend:
    """Resolve and validate the BrainzMash host immediately before each connect."""

    def __init__(self, delegate: Any) -> None:
        self._delegate = delegate

    @staticmethod
    def _validated_addresses(host: str, port: int) -> list[str]:
        try:
            infos = socket.getaddrinfo(
                host, port, type=socket.SOCK_STREAM, proto=socket.IPPROTO_TCP
            )
        except OSError:
            import httpcore

            raise httpcore.ConnectError("Could not resolve BrainzMash host") from None
        addresses: list[str] = []
        for _, _, _, _, sockaddr in infos:
            address = str(sockaddr[0]).split("%", 1)[0]
            try:
                parsed = ipaddress.ip_address(address)
            except ValueError:
                import httpcore

                raise httpcore.ConnectError(
                    "BrainzMash DNS returned an invalid address"
                ) from None
            if not parsed.is_global or parsed.is_multicast:
                import httpcore

                raise httpcore.ConnectError(
                    "BrainzMash DNS returned a non-public address"
                )
            if address not in addresses:
                addresses.append(address)
        if not addresses:
            import httpcore

            raise httpcore.ConnectError("BrainzMash DNS returned no addresses")
        return addresses

    async def connect_tcp(
        self,
        host: str,
        port: int,
        timeout: float | None = None,
        local_address: str | None = None,
        socket_options: Any = None,
    ) -> Any:
        import httpcore

        if host.casefold() != BRAINZMASH_HOST:
            raise httpcore.ConnectError(
                "BrainzMash transport received an unapproved host"
            )
        addresses = await asyncio.to_thread(self._validated_addresses, host, port)
        for address in addresses:
            try:
                # httpcore keeps ``host`` as the TLS SNI value while this
                # backend connects to the validated literal address.
                return await self._delegate.connect_tcp(
                    address,
                    port,
                    timeout=timeout,
                    local_address=local_address,
                    socket_options=socket_options,
                )
            except Exception:  # noqa: BLE001 - try another validated address
                continue
        raise httpcore.ConnectError("Could not connect to BrainzMash host") from None

    async def connect_unix_socket(
        self, path: str, timeout: float | None = None, socket_options: Any = None
    ) -> Any:
        return await self._delegate.connect_unix_socket(
            path, timeout=timeout, socket_options=socket_options
        )

    async def sleep(self, seconds: float) -> None:
        await self._delegate.sleep(seconds)


class BrainzMashTransport(httpx.AsyncHTTPTransport):
    """HTTPX transport that pins DNS results and keeps the approved hostname for TLS."""

    def __init__(self) -> None:
        super().__init__(
            verify=True,
            trust_env=False,
            http1=True,
            http2=False,
            limits=httpx.Limits(max_connections=1, max_keepalive_connections=1),
            retries=0,
        )
        self._pool._network_backend = _PinnedBrainzMashNetworkBackend(
            self._pool._network_backend
        )


def validate_brainzmash_url(url: str) -> str:
    """Validate the one server-owned BrainzMash origin and return its base URL."""
    try:
        parsed = urlsplit(url)
        hostname = parsed.hostname
        port = parsed.port
    except (AttributeError, TypeError, ValueError) as exc:
        raise ValueError("Invalid BrainzMash endpoint") from exc
    if (
        parsed.scheme != "https"
        or hostname != BRAINZMASH_HOST
        or port is not None
        or parsed.username is not None
        or parsed.password is not None
        or parsed.query
        or parsed.fragment
        or parsed.path.rstrip("/") != "/ws/2"
        or "%" in parsed.path
    ):
        raise ValueError("BrainzMash endpoint must be the approved HTTPS /ws/2 origin")
    return BRAINZMASH_ENDPOINT


def validate_brainzmash_path(path: str) -> str:
    """Allow only the MusicBrainz WS/2 entity paths used by this application."""
    if not isinstance(path, str) or not path.startswith("/") or "\\" in path:
        raise ValueError("Invalid BrainzMash API path")
    if "%" in path or "?" in path or "#" in path or "//" in path:
        raise ValueError("Invalid BrainzMash API path")
    segments = path.strip("/").split("/")
    if len(segments) not in (1, 2) or segments[0] not in _BRAINZMASH_ENTITY_PATHS:
        raise ValueError("Invalid BrainzMash API path")
    if len(segments) == 2 and (
        not segments[1]
        or segments[1] in {".", ".."}
        or any(ord(char) < 0x21 or ord(char) > 0x7E for char in segments[1])
        or not all(char.isalnum() or char == "-" for char in segments[1])
    ):
        raise ValueError("Invalid BrainzMash API path")

    return "/" + "/".join(segments)


def validate_brainzmash_request_url(url: str) -> None:
    """Reject request authority/path changes before an HTTPX wire attempt."""
    try:
        parsed = urlsplit(str(url))
        hostname = parsed.hostname
        port = parsed.port
    except (AttributeError, TypeError, ValueError) as exc:
        raise ValueError("Invalid BrainzMash request URL") from exc
    if (
        parsed.scheme != "https"
        or hostname is None
        or hostname.casefold() != BRAINZMASH_HOST
        or port is not None
        or parsed.username is not None
        or parsed.password is not None
        or parsed.fragment
        or not parsed.path.startswith("/ws/2/")
    ):
        raise ValueError("BrainzMash request authority is not approved")
    validate_brainzmash_path(parsed.path[len("/ws/2") :])
