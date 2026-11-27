"""Connector settings, read once from the environment when the server starts.

Identity (organization and user) and credentials come only from here, never from tool arguments.
"""

from __future__ import annotations

import ipaddress
import math
import os
import re
from collections.abc import Mapping
from dataclasses import dataclass, field
from typing import Literal
from urllib.parse import urlsplit

Mode = Literal["self-hosted", "hosted"]

#: Hosted developer tokens start with this prefix; a key with it selects hosted mode.
HOSTED_TOKEN_PREFIX = "mqd_"
HOSTED_DEFAULT_URL = "https://app.memquira.com"
#: The hosted API derives the account from the token. The SDK still needs an organization header and a user
#: id, so hosted mode sends this fixed placeholder for both; the hosted API refuses any other user id.
HOSTED_IDENTITY = "me"

DEFAULT_TIMEOUT = 60.0

_ORG_RE = re.compile(r"^[A-Za-z0-9_.-]{1,128}$")
_PLACEHOLDER_RE = re.compile(r"\$\{[^}]*\}|\$env:|%[A-Za-z_][A-Za-z0-9_]*%")
_TRUE = {"1", "true", "yes", "on"}
_FALSE = {"0", "false", "no", "off", ""}


class ConfigError(ValueError):
    """The connector's configuration is missing or unsafe. The message never contains the API key."""


@dataclass(frozen=True)
class Settings:
    mode: Mode
    base_url: str
    api_key: str = field(repr=False)
    org_id: str
    user_id: str
    timeout: float = DEFAULT_TIMEOUT
    read_only: bool = False

    def describe(self) -> str:
        """A one-line summary that is safe to log or print: never the key."""
        who = (
            "the account of the developer token" if self.mode == "hosted" else f"org={self.org_id} user={self.user_id}"
        )
        access = "read-only" if self.read_only else "read-write"
        return f"Memquira {self.mode} at {self.base_url} as {who} ({access})"


def _get(env: Mapping[str, str], name: str) -> str | None:
    value = env.get(name)
    if value is None:
        return None
    value = value.strip()
    if not value:
        return None
    if _PLACEHOLDER_RE.search(value):
        # Claude Code leaves an unset ${VAR} in place; Cursor uses ${env:VAR}. Say so instead of sending it.
        raise ConfigError(f"{name} looks like an unexpanded placeholder; set the variable it refers to")
    return value


def _bool(env: Mapping[str, str], name: str) -> bool:
    raw = (env.get(name) or "").strip().lower()
    if raw in _TRUE:
        return True
    if raw in _FALSE:
        return False
    raise ConfigError(f"{name} must be true or false")


def _is_loopback(host: str) -> bool:
    if host.lower() in {"localhost", "localhost."} or host.lower().endswith(".localhost"):
        return True
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return False


def check_url(url: str, *, allow_insecure_http: bool) -> str:
    """Validate the server URL and return it without a trailing slash."""
    try:
        parts = urlsplit(url)
        port = parts.port  # raises ValueError for a malformed port
    except ValueError as exc:
        raise ConfigError(f"MEMQUIRA_URL is not a valid URL: {exc}") from None
    del port
    if parts.scheme not in {"http", "https"}:
        raise ConfigError("MEMQUIRA_URL must start with https:// (or http:// for a local server)")
    if not parts.hostname:
        raise ConfigError("MEMQUIRA_URL has no host")
    if parts.username is not None or parts.password is not None:
        raise ConfigError("MEMQUIRA_URL must not contain credentials; put the key in MEMQUIRA_API_KEY")
    if parts.query or parts.fragment:
        raise ConfigError("MEMQUIRA_URL must not contain a query string or fragment")
    if parts.scheme == "http" and not _is_loopback(parts.hostname) and not allow_insecure_http:
        raise ConfigError(
            "MEMQUIRA_URL uses plain http:// to a non-local host, which would send the API key unencrypted. "
            "Use https://, or set MEMQUIRA_ALLOW_INSECURE_HTTP=true for a trusted private network."
        )
    return url.rstrip("/")


def _check_key(key: str) -> str:
    if len(key) < 8 or len(key) > 512 or any(c.isspace() or not c.isprintable() for c in key):
        raise ConfigError("MEMQUIRA_API_KEY is malformed (expected one printable token without spaces)")
    return key


def _check_user(user_id: str) -> str:
    if len(user_id) > 255 or any(not c.isprintable() for c in user_id):
        raise ConfigError("MEMQUIRA_USER_ID must be at most 255 printable characters")
    return user_id


def _timeout(env: Mapping[str, str]) -> float:
    raw = _get(env, "MEMQUIRA_TIMEOUT")
    if raw is None:
        return DEFAULT_TIMEOUT
    try:
        value = float(raw)
    except ValueError:
        raise ConfigError("MEMQUIRA_TIMEOUT must be a number of seconds") from None
    if not math.isfinite(value) or not 1 <= value <= 600:
        raise ConfigError("MEMQUIRA_TIMEOUT must be between 1 and 600 seconds")
    return value


def load_settings(env: Mapping[str, str] | None = None) -> Settings:
    """Read the settings from ``env`` (default: the process environment)."""
    env = os.environ if env is None else env
    api_key = _get(env, "MEMQUIRA_API_KEY")
    if api_key is None:
        raise ConfigError("MEMQUIRA_API_KEY is not set")
    api_key = _check_key(api_key)

    requested = (_get(env, "MEMQUIRA_MODE") or "auto").lower()
    if requested not in {"auto", "hosted", "self-hosted"}:
        raise ConfigError("MEMQUIRA_MODE must be auto, hosted or self-hosted")
    is_token = api_key.startswith(HOSTED_TOKEN_PREFIX)
    mode: Mode
    if requested == "auto":
        mode = "hosted" if is_token else "self-hosted"
    elif requested == "hosted":
        if not is_token:
            raise ConfigError(f"MEMQUIRA_MODE=hosted needs a developer token (starting with {HOSTED_TOKEN_PREFIX})")
        mode = "hosted"
    else:
        if is_token:
            raise ConfigError("a hosted developer token cannot be used with MEMQUIRA_MODE=self-hosted")
        mode = "self-hosted"

    allow_http = _bool(env, "MEMQUIRA_ALLOW_INSECURE_HTTP")
    timeout = _timeout(env)
    read_only = _bool(env, "MEMQUIRA_MCP_READ_ONLY")

    if mode == "hosted":
        for name in ("MEMQUIRA_ORG_ID", "MEMQUIRA_USER_ID"):
            if _get(env, name) is not None:
                raise ConfigError(
                    f"{name} is not used with a hosted developer token: the account comes from the token. "
                    f"Remove {name} from the configuration."
                )
        hosted_url = check_url(_get(env, "MEMQUIRA_URL") or HOSTED_DEFAULT_URL, allow_insecure_http=allow_http)
        return Settings("hosted", hosted_url, api_key, HOSTED_IDENTITY, HOSTED_IDENTITY, timeout, read_only)

    url = _get(env, "MEMQUIRA_URL")
    org_id = _get(env, "MEMQUIRA_ORG_ID")
    user_id = _get(env, "MEMQUIRA_USER_ID")
    if url is None or org_id is None or user_id is None:
        missing = [
            n for n, v in (("MEMQUIRA_URL", url), ("MEMQUIRA_ORG_ID", org_id), ("MEMQUIRA_USER_ID", user_id)) if not v
        ]
        raise ConfigError(f"{', '.join(missing)} not set (needed for a self-hosted Memquira server)")
    if not _ORG_RE.match(org_id):
        raise ConfigError("MEMQUIRA_ORG_ID must be 1-128 letters, digits, '.', '_' or '-'")
    return Settings(
        "self-hosted",
        check_url(url, allow_insecure_http=allow_http),
        api_key,
        org_id,
        _check_user(user_id),
        timeout,
        read_only,
    )
