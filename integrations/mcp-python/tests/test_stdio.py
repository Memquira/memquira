"""The installed entry point as a real subprocess over stdio: both protocol eras, clean stdout, logs on stderr."""

from __future__ import annotations

import json
import os
import socket
import subprocess
import sys
from typing import Any

import pytest
from mcp import Client
from mcp.client.stdio import StdioServerParameters

pytestmark = pytest.mark.anyio

SECRET = "mc_live_stdio_secret_value_42"


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return int(s.getsockname()[1])


def _env(**extra: str) -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if not k.startswith("MEMQUIRA_")}
    env.update(
        MEMQUIRA_URL=f"http://127.0.0.1:{_free_port()}",  # nothing listens here
        MEMQUIRA_API_KEY=SECRET,
        MEMQUIRA_ORG_ID="demo",
        MEMQUIRA_USER_ID="stdio-user",
        MEMQUIRA_MCP_LOG_LEVEL="DEBUG",
        MEMQUIRA_TIMEOUT="3",
    )
    env.update(extra)
    return env


@pytest.mark.parametrize("mode", ["auto", "legacy"])
async def test_stdio_both_protocol_eras(mode: Any) -> None:
    params = StdioServerParameters(command=sys.executable, args=["-m", "memquira_mcp"], env=_env())
    async with Client(params, mode=mode) as client:
        # A stray print to stdout would corrupt the stream and fail these calls.
        assert client.protocol_version == ("2026-07-28" if mode == "auto" else "2025-11-25")
        assert client.instructions and "long-term memory" in client.instructions
        names = [t.name for t in (await client.list_tools()).tools]
        assert names == ["remember", "search_memory", "get_context", "list_memories", "delete_memory"]
        result = await client.call_tool("search_memory", {"query": "anything"})
        assert result.is_error
        assert json.loads(result.content[0].text)["error"]["code"] == "UNREACHABLE"


def test_stdout_is_protocol_only_and_logs_go_to_stderr() -> None:
    log = _stderr_of_one_call()
    assert "tool=search_memory outcome=UNREACHABLE" in log
    assert SECRET not in log


def _stderr_of_one_call() -> str:
    """Drive the server with raw JSON-RPC (legacy handshake) and return its stderr."""
    messages = [
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "raw", "version": "0"},
            },
        },
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "search_memory", "arguments": {"query": "x"}},
        },
    ]
    proc = subprocess.Popen(
        [sys.executable, "-m", "memquira_mcp"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
        env=_env(),
    )
    assert proc.stdin is not None and proc.stdout is not None
    proc.stdin.write("".join(json.dumps(m) + "\n" for m in messages))
    proc.stdin.flush()
    lines = []
    while True:  # keep stdin open until the tool call is answered (closing it cancels in-flight calls)
        line = proc.stdout.readline()
        assert line, "server closed stdout before answering"
        lines.append(line)
        if json.loads(line).get("id") == 2:
            break
    # communicate() closes stdin itself (closing it first breaks Python 3.10 on POSIX: it flushes stdin).
    rest, stderr = proc.communicate(timeout=60)
    lines += [line for line in rest.splitlines() if line.strip()]
    # Every stdout line is a JSON-RPC message.
    assert all(json.loads(line)["jsonrpc"] == "2.0" for line in lines)
    assert all(SECRET not in line for line in lines)
    return stderr


def test_configuration_error_goes_to_stderr_with_exit_code_2() -> None:
    env = _env()
    del env["MEMQUIRA_USER_ID"]
    proc = subprocess.run([sys.executable, "-m", "memquira_mcp"], capture_output=True, text=True, env=env, timeout=60)
    assert proc.returncode == 2 and proc.stdout == ""
    assert "MEMQUIRA_USER_ID not set" in proc.stderr and SECRET not in proc.stderr


def test_check_command_reports_unreachable_server() -> None:
    proc = subprocess.run(
        [sys.executable, "-m", "memquira_mcp", "check"], capture_output=True, text=True, env=_env(), timeout=60
    )
    assert proc.returncode == 1
    assert "server: NOT reachable" in proc.stdout and SECRET not in proc.stdout + proc.stderr
