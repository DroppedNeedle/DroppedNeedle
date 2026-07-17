from __future__ import annotations

import argparse
import asyncio
import logging
import sys

from core.exceptions import AuthenticationError


class _IgnoreUnknownConfigKeys(logging.Filter):
    def filter(self, record: logging.LogRecord) -> bool:
        return record.msg != "Unknown config key '%s', ignoring"


def get_auth_service():
    from core.dependencies.auth_providers import get_auth_service as provider

    return provider()


def clear_all_singletons() -> None:
    from core.dependencies._registry import clear_all_singletons as clear

    clear()


def _get_auth_service_for_cli():
    config_logger = logging.getLogger("core.config")
    warning_filter = _IgnoreUnknownConfigKeys()
    config_logger.addFilter(warning_filter)
    try:
        return get_auth_service()
    finally:
        config_logger.removeFilter(warning_filter)


async def _create_recovery_code(username: str) -> int:
    try:
        service = _get_auth_service_for_cli()
        code, expires_at = await service.create_password_recovery_code_for_username(
            username
        )
    except AuthenticationError as exc:
        print(f"Error: {exc}", file=sys.stderr)
        return 1

    print("Password recovery code created.")
    print(f"Code: {code}")
    print(f"Expires: {expires_at}")
    print("Open /recover-password on your DroppedNeedle server to use it.")
    return 0


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="droppedneedle")
    commands = parser.add_subparsers(dest="command", required=True)
    recovery = commands.add_parser(
        "recovery-code",
        help="Create a one-time local-password recovery code",
    )
    recovery.add_argument("username")
    return parser


def main() -> int:
    args = _parser().parse_args()
    try:
        if args.command == "recovery-code":
            return asyncio.run(_create_recovery_code(args.username))
        return 2
    finally:
        clear_all_singletons()


if __name__ == "__main__":
    raise SystemExit(main())
