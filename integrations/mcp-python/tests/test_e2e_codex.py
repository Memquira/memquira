"""Cross-session acceptance through OpenAI Codex's own MCP client, with no model call.

Each "session" is a separate `codex app-server` process with its own thread. Codex reads a `config.toml` in a
temporary CODEX_HOME, launches memquira-mcp over stdio, forwards the credentials named in `env_vars`, lists the
tools, and runs each tool call through its MCP client (`mcpServer/tool/call`). No turn is started, so no model is
called, and the temporary CODEX_HOME holds no OpenAI credentials.

Skipped unless MEMQUIRA_E2E_CODEX (the codex executable) and MEMQUIRA_E2E_URL, MEMQUIRA_E2E_API_KEY and
MEMQUIRA_E2E_ORG_ID are set. Use a server with mock models: the test writes memories and erases its test user.

What this does not cover: whether the model chooses the tools, how Codex applies approval modes to model-initiated
calls, and how it uses the server instructions. Those need an interactive Codex session.
"""

from __future__ import annotations

import json
import os
import queue
import subprocess
import sys
import threading
import uuid
from collections.abc import Iterator
from contextlib import contextmanager, suppress
from pathlib import Path
from typing import Any

import memquira
import pytest

CODEX = os.environ.get("MEMQUIRA_E2E_CODEX")
URL = os.environ.get("MEMQUIRA_E2E_URL")
KEY = os.environ.get("MEMQUIRA_E2E_API_KEY")
ORG = os.environ.get("MEMQUIRA_E2E_ORG_ID")
DELETE_FIX = os.environ.get("MEMQUIRA_E2E_EXPECT_DELETE_FIX") == "1"

pytestmark = [
    pytest.mark.e2e,
    pytest.mark.skipif(not (CODEX and URL and KEY and ORG), reason="MEMQUIRA_E2E_CODEX or MEMQUIRA_E2E_* not set"),
]

TOOLS = {"remember", "search_memory", "get_context", "list_memories", "delete_memory"}
READ_ONLY = {"search_memory", "get_context", "list_memories"}
CREDENTIALS = ["MEMQUIRA_URL", "MEMQUIRA_API_KEY", "MEMQUIRA_ORG_ID", "MEMQUIRA_USER_ID"]


def toml_string(value: str) -> str:
    return json.dumps(value)  # a JSON string is a valid TOML basic string


def write_config(home: Path) -> None:
    """The documented self-hosted configuration: credentials by name only, never as values in the file."""
    home.mkdir(parents=True, exist_ok=True)
    (home / "config.toml").write_text(
        "\n".join(
            [
                "[mcp_servers.memquira]",
                f"command = {toml_string(sys.executable)}",
                'args = ["-m", "memquira_mcp", "serve"]',
                f"env_vars = {json.dumps(CREDENTIALS)}",
                "startup_timeout_sec = 30",
                "tool_timeout_sec = 90",
                "",
                "[mcp_servers.memquira.tools.delete_memory]",
                'approval_mode = "prompt"',
                "",
            ]
        ),
        encoding="utf-8",
    )


class CodexSession:
    """One `codex app-server` process speaking JSON-RPC on stdio."""

    def __init__(self, home: Path, user_id: str) -> None:
        assert CODEX and URL and KEY and ORG
        env = {k: v for k, v in os.environ.items() if not k.startswith(("MEMQUIRA_", "OPENAI_", "CODEX_"))}
        env.update(
            {
                "CODEX_HOME": str(home),
                "MEMQUIRA_URL": URL,
                "MEMQUIRA_API_KEY": KEY,
                "MEMQUIRA_ORG_ID": ORG,
                "MEMQUIRA_USER_ID": user_id,
            }
        )
        self.proc = subprocess.Popen(
            [CODEX, "app-server"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            env=env,
        )
        self.lines: queue.Queue[bytes] = queue.Queue()
        threading.Thread(target=self._read, daemon=True).start()
        self.next_id = 0
        self.server_requests: list[dict[str, Any]] = []
        self.request("initialize", {"clientInfo": {"name": "memquira-acceptance", "version": "1"}})
        self._send({"jsonrpc": "2.0", "method": "initialized"})
        started = self.request("thread/start", {"ephemeral": True})
        self.thread_id: str = started["thread"]["id"]

    def _read(self) -> None:
        assert self.proc.stdout is not None
        for line in self.proc.stdout:
            self.lines.put(line)

    def _send(self, message: dict[str, Any]) -> None:
        assert self.proc.stdin is not None
        self.proc.stdin.write((json.dumps(message) + "\n").encode())
        self.proc.stdin.flush()

    def request(self, method: str, params: dict[str, Any], timeout: float = 120) -> Any:
        self.next_id += 1
        rid = self.next_id
        self._send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        while True:
            message = json.loads(self.lines.get(timeout=timeout))
            if message.get("id") == rid and "method" not in message:
                if "error" in message:
                    raise AssertionError(f"{method}: {message['error']}")
                return message["result"]
            if "method" in message and "id" in message:
                # A request from Codex to this client (for example an approval): record it and decline.
                self.server_requests.append(message)
                self._send({"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32601, "message": "test"}})

    def status(self) -> dict[str, Any]:
        servers = self.request("mcpServerStatus/list", {})["data"]
        (memquira_status,) = [s for s in servers if s["name"] == "memquira"]
        return dict(memquira_status)

    def call(self, tool: str, **arguments: Any) -> dict[str, Any]:
        result = self.request(
            "mcpServer/tool/call",
            {"server": "memquira", "threadId": self.thread_id, "tool": tool, "arguments": arguments},
        )
        assert not result.get("isError"), result
        return dict(result["structuredContent"])

    def close(self) -> None:
        self.proc.terminate()
        with suppress(subprocess.TimeoutExpired):
            self.proc.wait(timeout=10)


@contextmanager
def session(home: Path, user_id: str) -> Iterator[CodexSession]:
    s = CodexSession(home, user_id)
    try:
        yield s
    finally:
        s.close()


def recalled(s: CodexSession, query: str) -> str:
    """Everything the read tools return for the user, as one text."""
    context = s.call("get_context", query=query)["context"]
    found = [r["content"] for r in s.call("search_memory", query=query)["results"]]
    listed = [m["content"] for m in s.call("list_memories", limit=50)["memories"]]
    return "\n".join([context, *found, *listed])


def test_remember_recall_list_delete_across_codex_sessions(tmp_path: Path) -> None:
    home = tmp_path / "codex-home"
    write_config(home)
    user_id = f"codex-e2e-{uuid.uuid4().hex[:8]}"
    fact = "Our deployment region is eu-west-3."
    try:
        # Session 1: Codex discovers the five tools with their read-only hints, then stores one memory.
        with session(home, user_id) as s1:
            status = s1.status()
            assert set(status["tools"]) == TOOLS
            for name, tool in status["tools"].items():
                assert tool["annotations"]["readOnlyHint"] is (name in READ_ONLY), name
            assert status["serverInfo"]["name"] == "memquira"
            stored = s1.call("remember", content=fact)
            assert stored["status"] == "stored", stored

        # Session 2: a new process recalls it through every read tool and finds its id.
        with session(home, user_id) as s2:
            assert "eu-west-3" in s2.call("get_context", query="Which region do we deploy to?")["context"]
            hits = s2.call("search_memory", query="deployment region")["results"]
            (memory_id,) = [h["id"] for h in hits if "eu-west-3" in h["content"]]
            assert any(m["id"] == memory_id for m in s2.call("list_memories", limit=50)["memories"])

        # Session 3: delete it; it is gone at once.
        with session(home, user_id) as s3:
            deleted = s3.call("delete_memory", memory_id=memory_id)
            assert deleted["deleted"] is True
            after = s3.call("search_memory", query="deployment region")["results"]
            assert not any("eu-west-3" in r["content"] for r in after), after

        # Session 4: a fresh process does not get it back from any read tool (with the deleted-memory fix, not even
        # through the message it came from).
        with session(home, user_id) as s4:
            text = recalled(s4, "Which region do we deploy to?")
            if DELETE_FIX:
                assert "eu-west-3" not in text, text
            else:
                assert "eu-west-3" not in "\n".join(
                    [m["content"] for m in s4.call("list_memories", limit=50)["memories"]]
                )
    finally:
        assert URL and KEY and ORG
        with memquira.Memquira(URL, KEY, ORG) as client, suppress(memquira.MemquiraError):
            client.forget_user(user_id)


def test_the_config_holds_no_credential_values(tmp_path: Path) -> None:
    home = tmp_path / "codex-home"
    write_config(home)
    text = (home / "config.toml").read_text(encoding="utf-8")
    assert KEY and KEY not in text
    assert "env_vars" in text and "env = " not in text
