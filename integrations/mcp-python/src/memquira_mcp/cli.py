"""Command line: ``memquira-mcp`` serves MCP over stdio; ``memquira-mcp check`` tests the configuration."""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import logging
import os
import sys
from collections.abc import Sequence

import httpx
import memquira

from ._version import __version__
from .config import ConfigError, Settings, load_settings
from .server import MAX_RETRY_WAIT, make_http_client, serve_stdio


class RedactFilter(logging.Filter):
    """Replaces the API key in any log record, as a second line of defence: nothing should log it."""

    def __init__(self, secret: str) -> None:
        super().__init__()
        self._secret = secret

    def filter(self, record: logging.LogRecord) -> bool:
        if self._secret:
            message = record.getMessage()
            if self._secret in message:
                record.msg = message.replace(self._secret, "[REDACTED]")
                record.args = None
        return True


def setup_logging(secret: str | None) -> None:
    level = os.environ.get("MEMQUIRA_MCP_LOG_LEVEL", "WARNING").upper()
    handler = logging.StreamHandler(sys.stderr)  # stdout belongs to the MCP protocol
    handler.setFormatter(logging.Formatter("%(asctime)s %(levelname)s %(name)s: %(message)s"))
    if secret:
        handler.addFilter(RedactFilter(secret))
    root = logging.getLogger()
    root.handlers[:] = [handler]
    root.setLevel(level if isinstance(logging.getLevelName(level), int) else "WARNING")
    # httpx logs every request URL at INFO; keep it quiet unless debugging.
    logging.getLogger("httpx").setLevel(logging.WARNING if root.level > logging.DEBUG else logging.DEBUG)


async def _check(settings: Settings) -> int:
    """Read-only checks that cost nothing: server version and one page of memories (auth + read scope)."""
    print(f"memquira-mcp {__version__}")
    print(f"configuration: {settings.describe()}")
    http = make_http_client(settings)
    try:
        return await _run_checks(settings, http)
    finally:
        await http.aclose()


async def _run_checks(settings: Settings, http: httpx.AsyncClient) -> int:
    async with memquira.AsyncMemquira(
        settings.base_url,
        settings.api_key,
        settings.org_id,
        max_retries=1,
        max_retry_wait=MAX_RETRY_WAIT,
        http_client=http,
    ) as client:
        try:
            version = await client.version()
            print(f"server: reachable, version {version.package_version or 'unknown'}")
        except memquira.MemquiraError as exc:
            print(f"server: NOT reachable or not a Memquira API ({type(exc).__name__}: {exc})")
            return 1
        try:
            page = await client.for_user(settings.user_id).list_memories(limit=1)
            more = " (more available)" if page.next_cursor else ""
            print(f"credentials: accepted; memories readable ({len(page.memories)} shown{more})")
        except memquira.AuthenticationError:
            print("credentials: REJECTED (401) - check MEMQUIRA_API_KEY")
            return 1
        except memquira.PermissionDeniedError as exc:
            print(f"credentials: accepted but NOT allowed to read memories (403 {exc.code or ''})")
            return 1
        except memquira.MemquiraError as exc:
            print(f"credentials: check failed ({type(exc).__name__}: {exc})")
            return 1
    print("ok: add this server to your MCP client (see the README)")
    return 0


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="memquira-mcp",
        description="Memquira memory tools for MCP clients (stdio), configured by MEMQUIRA_* environment variables.",
    )
    parser.add_argument("--version", action="version", version=f"memquira-mcp {__version__}")
    parser.add_argument(
        "command",
        nargs="?",
        choices=["serve", "check"],
        default="serve",
        help="serve (default): run the MCP server on stdin/stdout; check: test the configuration and exit",
    )
    args = parser.parse_args(argv)
    try:
        settings = load_settings()
    except ConfigError as exc:
        setup_logging(None)
        print(f"memquira-mcp: configuration error: {exc}", file=sys.stderr)
        return 2
    setup_logging(settings.api_key)
    if args.command == "check":
        return asyncio.run(_check(settings))
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(serve_stdio(settings))
    return 0


if __name__ == "__main__":  # pragma: no cover
    sys.exit(main())
