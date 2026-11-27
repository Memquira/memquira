from __future__ import annotations

import logging
from typing import Any

import pytest

from memquira_mcp.cli import RedactFilter
from memquira_mcp.config import HOSTED_DEFAULT_URL, ConfigError, load_settings

KEY = "mc_live_0123456789abcdef"
TOKEN = "mqd_abcdefghijklmnopqrstuvwxyz012345"


def env(**values: str) -> dict[str, str]:
    base = {
        "MEMQUIRA_URL": "http://127.0.0.1:8080",
        "MEMQUIRA_API_KEY": KEY,
        "MEMQUIRA_ORG_ID": "demo",
        "MEMQUIRA_USER_ID": "alice",
    }
    base.update(values)
    return {k: v for k, v in base.items() if v != "<unset>"}


def test_self_hosted() -> None:
    s = load_settings(env(MEMQUIRA_URL="https://memory.example.com/base/"))
    assert (s.mode, s.base_url, s.org_id, s.user_id, s.read_only) == (
        "self-hosted",
        "https://memory.example.com/base",
        "demo",
        "alice",
        False,
    )
    assert KEY not in repr(s) and KEY not in s.describe()


def test_hosted_uses_token_identity_and_default_url() -> None:
    s = load_settings({"MEMQUIRA_API_KEY": TOKEN})
    assert (s.mode, s.base_url, s.org_id, s.user_id) == ("hosted", HOSTED_DEFAULT_URL, "me", "me")
    assert TOKEN not in s.describe() and "account of the developer token" in s.describe()


@pytest.mark.parametrize("name", ["MEMQUIRA_USER_ID", "MEMQUIRA_ORG_ID"])
def test_hosted_refuses_local_identity(name: str) -> None:
    with pytest.raises(ConfigError, match=f"{name} is not used with a hosted developer token"):
        load_settings({"MEMQUIRA_API_KEY": TOKEN, name: "bob"})


@pytest.mark.parametrize(
    ("values", "message"),
    [
        ({"MEMQUIRA_API_KEY": "<unset>"}, "MEMQUIRA_API_KEY is not set"),
        ({"MEMQUIRA_API_KEY": "   "}, "MEMQUIRA_API_KEY is not set"),
        ({"MEMQUIRA_API_KEY": "${MEMQUIRA_KEY}"}, "unexpanded placeholder"),
        ({"MEMQUIRA_API_KEY": "${env:MEMQUIRA_KEY}"}, "unexpanded placeholder"),
        ({"MEMQUIRA_API_KEY": "short"}, "malformed"),
        ({"MEMQUIRA_API_KEY": "two words here"}, "malformed"),
        ({"MEMQUIRA_USER_ID": "<unset>"}, "MEMQUIRA_USER_ID not set"),
        ({"MEMQUIRA_URL": "<unset>", "MEMQUIRA_ORG_ID": "<unset>"}, "MEMQUIRA_URL, MEMQUIRA_ORG_ID not set"),
        ({"MEMQUIRA_ORG_ID": "bad org!"}, "MEMQUIRA_ORG_ID must be"),
        ({"MEMQUIRA_URL": "http://memory.example.com"}, "plain http://"),
        ({"MEMQUIRA_URL": "http://memory.lan.example:8080"}, "plain http://"),
        ({"MEMQUIRA_URL": "ftp://localhost"}, "must start with https://"),
        ({"MEMQUIRA_URL": "https://user:pw@memory.example.com"}, "must not contain credentials"),
        ({"MEMQUIRA_URL": "https://memory.example.com/?key=1"}, "query string"),
        ({"MEMQUIRA_URL": "https://"}, "has no host"),
        ({"MEMQUIRA_URL": "https://h:99999"}, "not a valid URL"),
        ({"MEMQUIRA_MODE": "cloud"}, "MEMQUIRA_MODE must be"),
        ({"MEMQUIRA_MODE": "hosted"}, "needs a developer token"),
        ({"MEMQUIRA_MODE": "self-hosted", "MEMQUIRA_API_KEY": TOKEN}, "cannot be used with MEMQUIRA_MODE=self-hosted"),
        ({"MEMQUIRA_TIMEOUT": "0"}, "between 1 and 600"),
        ({"MEMQUIRA_TIMEOUT": "nan"}, "between 1 and 600"),
        ({"MEMQUIRA_TIMEOUT": "soon"}, "number of seconds"),
        ({"MEMQUIRA_MCP_READ_ONLY": "maybe"}, "true or false"),
        (
            {
                "MEMQUIRA_API_KEY": TOKEN,
                "MEMQUIRA_URL": "http://app.example.com",
                "MEMQUIRA_ORG_ID": "<unset>",
                "MEMQUIRA_USER_ID": "<unset>",
            },
            "plain http://",
        ),
    ],
)
def test_rejected_configurations(values: dict[str, str], message: str) -> None:
    with pytest.raises(ConfigError, match=message) as info:
        load_settings(env(**values))
    assert KEY not in str(info.value) and TOKEN not in str(info.value)


@pytest.mark.parametrize(
    "url", ["http://localhost:8080", "http://127.0.0.1", "http://[::1]:8080", "http://api.localhost"]
)
def test_plain_http_allowed_for_loopback(url: str) -> None:
    assert load_settings(env(MEMQUIRA_URL=url)).base_url == url


def test_plain_http_escape_hatch_and_options() -> None:
    s = load_settings(
        env(
            MEMQUIRA_URL="http://memory.internal.example:8080",
            MEMQUIRA_ALLOW_INSECURE_HTTP="true",
            MEMQUIRA_TIMEOUT="12.5",
            MEMQUIRA_MCP_READ_ONLY="yes",
        )
    )
    assert (s.base_url, s.timeout, s.read_only) == ("http://memory.internal.example:8080", 12.5, True)


def test_redact_filter() -> None:
    record: Any = logging.LogRecord("x", logging.INFO, __file__, 1, "key=%s end", (KEY,), None)
    assert RedactFilter(KEY).filter(record)
    assert record.getMessage() == "key=[REDACTED] end"
