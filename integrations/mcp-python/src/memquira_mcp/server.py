"""The MCP server: tool listing and dispatch over one bound Memquira identity."""

from __future__ import annotations

import json
import logging
import time
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from dataclasses import dataclass
from typing import Any

import httpx
import mcp.types as types
import memquira
from mcp.server import Server
from mcp.server.stdio import stdio_server
from mcp.shared.exceptions import MCPError

from . import errors, tools
from ._version import __version__
from .config import Settings
from .guidance import INSTRUCTIONS
from .schema import ArgumentError

log = logging.getLogger("memquira_mcp")

#: Retries happen inside one tool call; keep the worst case short for an interactive client.
MAX_RETRIES = 2
MAX_RETRY_WAIT = 10.0


@dataclass
class _State:
    memory: tools.UserMemory


def user_agent_hook(product: str) -> Any:
    """An httpx request hook that names this connector in the User-Agent (no user or machine data)."""

    async def hook(request: httpx.Request) -> None:
        current = request.headers.get("User-Agent", "")
        request.headers["User-Agent"] = f"{product} {current}".strip()

    return hook


def make_http_client(settings: Settings, transport: httpx.AsyncBaseTransport | None = None) -> httpx.AsyncClient:
    """The HTTP client of the connector. ``transport`` replaces the network in tests."""
    return httpx.AsyncClient(
        timeout=settings.timeout,
        # Never follow redirects: a redirect could carry the Authorization header to another host.
        follow_redirects=False,
        verify=True,
        transport=transport,
        event_hooks={"request": [user_agent_hook(f"memquira-mcp/{__version__}")]},
    )


def _tool_definition(spec: tools.ToolSpec) -> types.Tool:
    a = spec.annotations
    return types.Tool(
        name=spec.name,
        title=spec.title,
        description=spec.description,
        input_schema=spec.input_schema,
        output_schema=spec.output_schema,
        annotations=types.ToolAnnotations(
            title=spec.title,
            read_only_hint=a["readOnlyHint"],
            destructive_hint=a["destructiveHint"],
            idempotent_hint=a["idempotentHint"],
            open_world_hint=a["openWorldHint"],
        ),
        # Claude Code otherwise defers MCP tools behind its tool search, and a model that cannot see the tools
        # falls back to its built-in memory. Other clients ignore this key.
        _meta={"anthropic/alwaysLoad": True},
    )


def _ok(payload: dict[str, Any]) -> types.CallToolResult:
    return types.CallToolResult(
        content=[types.TextContent(type="text", text=json.dumps(payload, ensure_ascii=False))],
        structured_content=payload,
    )


def _error(info: dict[str, Any]) -> types.CallToolResult:
    return types.CallToolResult(
        content=[types.TextContent(type="text", text=json.dumps({"error": info}, ensure_ascii=False))],
        is_error=True,
    )


def build_server(settings: Settings, *, http_client: httpx.AsyncClient | None = None) -> Server[Any]:
    """The MCP server for one configured identity. ``http_client`` is for tests (a mocked transport)."""
    specs = tools.available_tools(settings.read_only)
    by_name = {s.name: s for s in specs}
    definitions = [_tool_definition(s) for s in specs]

    @asynccontextmanager
    async def lifespan(_server: Server[Any]) -> AsyncIterator[_State]:
        http = http_client or make_http_client(settings)
        client = memquira.AsyncMemquira(
            settings.base_url,
            settings.api_key,
            settings.org_id,
            max_retries=MAX_RETRIES,
            max_retry_wait=MAX_RETRY_WAIT,
            http_client=http,
        )
        try:
            # for_user fixes the user id for the whole process: no tool can change it.
            yield _State(memory=client.for_user(settings.user_id))
        finally:
            if http_client is None:
                await http.aclose()

    async def on_list_tools(_ctx: Any, _params: Any) -> types.ListToolsResult:
        return types.ListToolsResult(tools=definitions)

    async def on_call_tool(ctx: Any, params: types.CallToolRequestParams) -> types.CallToolResult:
        spec = by_name.get(params.name)
        if spec is None:
            raise MCPError(types.INVALID_PARAMS, f"Unknown tool: {params.name[:64]}")
        started = time.monotonic()
        outcome = "cancelled"  # stays so only when the call is cancelled mid-flight
        request_id: str | None = None
        try:
            try:
                args = tools.check_arguments(spec, params.arguments)
            except ArgumentError as exc:
                outcome = "invalid_arguments"
                return _error({"tool": spec.name, "code": "INVALID_ARGUMENTS", "message": str(exc), "retryable": False})
            if spec is tools.REMEMBER:
                # Fix the idempotency key before the first attempt, so a retry (ours or the model's) cannot store twice.
                request_id = args.setdefault("request_id", tools.new_request_id())
            state: _State = ctx.lifespan_context
            try:
                payload = await spec.handler(state.memory, args)
            except memquira.MemquiraError as exc:
                info = errors.describe(exc, tool=spec.name, request_id=request_id)
                outcome = str(info.get("code") or info.get("status") or "error")
                return _error(info)
            except Exception:
                log.exception("tool %s failed", spec.name)
                outcome = "internal"
                return _error(errors.describe(RuntimeError(), tool=spec.name, request_id=request_id))
            outcome = "ok"
            return _ok(payload)
        finally:
            log.info("tool=%s outcome=%s duration_ms=%d", spec.name, outcome, (time.monotonic() - started) * 1000)

    return Server(
        "memquira",
        version=__version__,
        title="Memquira",
        description="Long-term memory for AI assistants, backed by a Memquira server.",
        instructions=INSTRUCTIONS,
        website_url="https://github.com/Memquira/memquira",
        lifespan=lifespan,
        on_list_tools=on_list_tools,
        on_call_tool=on_call_tool,
    )


async def serve_stdio(settings: Settings) -> None:
    server = build_server(settings)
    log.info("starting: %s", settings.describe())
    async with stdio_server() as (read_stream, write_stream):
        await server.run(read_stream, write_stream, server.create_initialization_options())
