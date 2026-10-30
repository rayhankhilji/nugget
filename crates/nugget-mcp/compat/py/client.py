"""MCP compatibility check with the official Python SDK (`mcp`, pinned in requirements.txt).

Runs against a running nuggetd over the stdio bridge (`nuggetd mcp --client <id>`, launched
exactly as the `claude-desktop` setup snippet says) and loopback streamable HTTP (the
`generic-http` snippet's URL and Authorization header).

Run through the fixture, which starts the daemon and writes the connection details:
    node ../fixture.mjs -- python client.py
Prints one line per check and a summary; exits nonzero when any check fails.
"""

from __future__ import annotations

import json
import os
import sys
import traceback
import warnings
from collections.abc import Awaitable, Callable
from importlib.metadata import version
from typing import Any

import anyio
import httpx2
import mcp_types as types
from mcp import Client, StdioServerParameters
from mcp.client.streamable_http import streamable_http_client
from mcp.shared.exceptions import MCPDeprecationWarning, MCPError
from mcp_types.version import HANDSHAKE_PROTOCOL_VERSIONS, MODERN_PROTOCOL_VERSIONS

# The SDK flags handshake-era features (ping) as deprecated; they are still part of 2025-11-25.
warnings.simplefilter("ignore", MCPDeprecationWarning)

TOOLS = ["search", "get_context", "find_entities", "get_entity", "get_neighbourhood", "get_item", "recent_changes"]
INVALID_PARAMS = -32602
RESOURCE_NOT_FOUND = -32002
TIMEOUT = 30.0

#: Revisions Nugget (rmcp 3.8.0) speaks.
NUGGET_REVISIONS = ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25", "2026-07-28"]
#: The two newest revisions this SDK and Nugget share, newest first.
REVISIONS = sorted(
    (v for v in (*HANDSHAKE_PROTOCOL_VERSIONS, *MODERN_PROTOCOL_VERSIONS) if v in NUGGET_REVISIONS),
    reverse=True,
)[:2]

fixture_path = os.environ.get("NUGGET_MCP_COMPAT_FIXTURE")
if not fixture_path:
    print("set NUGGET_MCP_COMPAT_FIXTURE (run through ../fixture.mjs)", file=sys.stderr)
    sys.exit(2)
with open(fixture_path, encoding="utf-8") as f:
    FX: dict[str, Any] = json.load(f)

# ---- reporting ----

results: list[tuple[str, bool, str]] = []
section = ""


async def check(name: str, fn: Callable[[], Awaitable[Any]]) -> bool:
    label = f"{section} {name}".strip()
    try:
        with anyio.fail_after(120):
            await fn()
    except Exception as e:  # noqa: BLE001 - every failure is reported, none stops the run
        msg = f"{type(e).__name__}: {e}"
        if os.environ.get("COMPAT_VERBOSE"):
            traceback.print_exc()
        results.append((label, False, msg))
        print(f"FAIL {label}: {msg}", flush=True)
        return False
    results.append((label, True, ""))
    print(f"PASS {label}", flush=True)
    return True


def expect(cond: bool, msg: str) -> None:
    if not cond:
        raise AssertionError(msg)


async def raises(coro: Awaitable[Any], code: int | None, what: str) -> BaseException:
    try:
        await coro
    except MCPError as e:
        expect(code is None or e.code == code, f"{what}: expected code {code}, got {e.code} ({e})")
        return e
    except Exception as e:  # noqa: BLE001
        expect(code is None, f"{what}: expected MCP error {code}, got {type(e).__name__}: {e}")
        return e
    raise AssertionError(f"{what}: expected an error, got success")


def text_of(r: types.CallToolResult) -> str:
    return "\n".join(c.text for c in r.content if isinstance(c, types.TextContent))


# ---- connections ----


def stdio(conn: dict[str, Any], env: dict[str, str] | None = None) -> StdioServerParameters:
    s = conn["stdio"]
    return StdioServerParameters(command=s["command"], args=s["args"], env={**s["env"], **(env or {})})


def http(conn: dict[str, Any], headers: dict[str, str] | None = None):  # noqa: ANN201
    h = conn["http"]["headers"] if headers is None else headers
    return streamable_http_client(conn["http"]["url"], http_client=httpx2.AsyncClient(headers=h, timeout=TIMEOUT))


def mode_for(revision: str) -> str:
    # 2026-07-28 connects as hosts do: `auto` sends server/discover (and would fall back to the
    # initialize handshake on a server without it, which the negotiated-version check catches).
    return "auto" if revision in MODERN_PROTOCOL_VERSIONS else "legacy"


def client(kind: str, conn: dict[str, Any], revision: str, mode: str | None = None) -> Client:
    server = stdio(conn) if kind == "stdio" else http(conn)
    return Client(
        server,
        mode=mode or mode_for(revision),
        read_timeout_seconds=TIMEOUT,
        client_info=types.Implementation(name="nugget-compat-py", version="1.0.0"),
    )


# ---- the leg: one transport at one revision ----


async def leg(kind: str, conn: dict[str, Any], revision: str) -> None:
    global section
    section = f"[py {kind} {revision}]"
    connect = "initialize" if mode_for(revision) == "legacy" else "connect (per-request envelope)"
    phase = connect
    try:
        # The client's task group must be entered and left in the same scope, so the connection
        # wraps the whole leg rather than living inside one check.
        async with client(kind, conn, revision) as c:
            phase = "session"

            async def init() -> None:
                expect(c.protocol_version == revision, f"negotiated {c.protocol_version}, wanted {revision}")
                expect(c.server_info is not None and c.server_info.name == "nugget", f"server {c.server_info}")
                caps = c.server_capabilities
                expect(caps.tools is not None and caps.resources is not None, f"capabilities {caps}")
                expect("untrusted" in (c.instructions or ""), "instructions carry the safety note")

            if await check(connect, init):
                await body(kind, c, revision)
            phase = "close"
    except Exception as e:  # noqa: BLE001
        label = f"{section} {phase}"
        if os.environ.get("COMPAT_VERBOSE"):
            traceback.print_exc()
        results.append((label, False, f"{type(e).__name__}: {e}"))
        print(f"FAIL {label}: {type(e).__name__}: {e}", flush=True)
        return
    results.append((f"{section} close", True, ""))
    print(f"PASS {section} close", flush=True)


async def body(kind: str, c: Client, revision: str) -> None:
    modern = revision in MODERN_PROTOCOL_VERSIONS
    if not modern:
        await check("ping", lambda: c.send_ping())

    async def list_tools() -> None:
        r = await c.list_tools()
        names = sorted(t.name for t in r.tools)
        expect(names == sorted(TOOLS), f"tools {names}")
        for t in r.tools:
            expect(t.input_schema.get("type") == "object", f"{t.name}: inputSchema")
            expect(t.annotations is not None and t.annotations.read_only_hint is True, f"{t.name}: readOnlyHint")
        expect(r.next_cursor is None, "tools/list is a single page")

    await check("tools/list", list_tools)

    async def list_tools_cursor() -> None:
        r = await c.session.list_tools(params=types.PaginatedRequestParams(cursor="anything"))
        expect(len(r.tools) == len(TOOLS), f"got {len(r.tools)} tools")

    await check("tools/list ignores a cursor", list_tools_cursor)

    state: dict[str, Any] = {}

    async def call(name: str, args: dict[str, Any]) -> types.CallToolResult:
        return await c.call_tool(name, args)

    async def search() -> None:
        r = await call("search", {"query": FX["needle"], "limit": 5})
        expect(not r.is_error, text_of(r))
        hits = (r.structured_content or {}).get("hits", [])
        hit = next((h for h in hits if "Falcon" in (h.get("snippet") or "")), hits[0] if hits else None)
        expect(hit is not None and hit.get("uri"), f"no hit: {text_of(r)[:300]}")
        expect("untrusted-content" in text_of(r), "content is wrapped as untrusted")
        state["hit"] = hit

    async def get_context() -> None:
        r = await call("get_context", {"query": "When is the Falcon launch review?", "budget_tokens": 500})
        expect(not r.is_error, text_of(r))
        expect("[1]" in text_of(r), "numbered passages")

    async def find_entities() -> None:
        r = await call("find_entities", {"text": "Compat Project 001", "kind": "project", "limit": 5})
        expect(not r.is_error, text_of(r))
        ents = (r.structured_content or {}).get("entities", [])
        expect(bool(ents) and ents[0].get("id"), f"no entity: {text_of(r)[:300]}")
        state["entity"] = ents[0]

    async def get_entity() -> None:
        r = await call("get_entity", {"id": state["entity"]["id"]})
        expect(not r.is_error, text_of(r))
        expect("Compat Project 001" in text_of(r), "entity page names the entity")

    async def get_neighbourhood() -> None:
        r = await call("get_neighbourhood", {"id": state["entity"]["id"], "depth": 1})
        expect(not r.is_error, text_of(r))
        expect(len((r.structured_content or {}).get("nodes", [])) >= 1, "at least the centre node")

    async def get_item() -> None:
        h = state["hit"]
        r = await call("get_item", {"source": h["source"], "key": h["key"], "max_chars": 2000})
        expect(not r.is_error, text_of(r))
        expect("Falcon" in text_of(r), "item text")

    async def recent_changes() -> None:
        r = await call("recent_changes", {"limit": 5})
        expect(not r.is_error, text_of(r))
        expect(len((r.structured_content or {}).get("entities", [])) > 0, "recent entities")

    for name, fn in [
        ("search", search),
        ("get_context", get_context),
        ("find_entities", find_entities),
        ("get_entity", get_entity),
        ("get_neighbourhood", get_neighbourhood),
        ("get_item", get_item),
        ("recent_changes", recent_changes),
    ]:
        await check(f"call {name}", fn)

    bad: dict[str, dict[str, Any]] = {
        "search": {"limit": 3},
        "get_context": {"query": 42},
        "find_entities": {"text": "x", "kind": "spaceship"},
        "get_entity": {},
        "get_neighbourhood": {"id": "x", "depth": "deep"},
        "get_item": {"source": "only-source"},
        "recent_changes": {"since": "yesterday-ish"},
    }
    for name in TOOLS:

        async def invalid(name: str = name) -> None:
            r = await call(name, bad[name])
            expect(r.is_error is True, f"expected isError, got {r.model_dump_json()[:300]}")

        await check(f"call {name} with invalid args", invalid)

    async def missing_entity() -> None:
        r = await call("get_entity", {"id": "ent_does_not_exist"})
        expect(r.is_error is True, "expected isError")

    await check("call get_entity for a missing entity", missing_entity)
    await check("unknown tool is -32602", lambda: raises(call("rm_rf", {}), INVALID_PARAMS, "unknown tool"))

    resources: list[types.Resource] = []

    async def list_resources() -> None:
        cursor: str | None = None
        pages = 0
        while True:
            r = await c.list_resources(cursor=cursor)
            resources.extend(r.resources)
            pages += 1
            expect(pages < 20, "too many pages")
            cursor = r.next_cursor
            if not cursor:
                break
        uris = {r.uri for r in resources}
        expect(pages > 1, f"expected more than one page, got {pages}")
        expect(len(uris) == len(resources), "no duplicates across pages")
        expect(len(resources) >= FX["projects"], f"listed {len(resources)}, want >= {FX['projects']}")
        expect(all(str(r.uri).startswith("nugget://entity/") for r in resources), "entity resources")

    await check("resources/list follows nextCursor", list_resources)
    await check(
        "resources/list rejects a garbage cursor",
        lambda: raises(c.list_resources(cursor="not-a-cursor"), INVALID_PARAMS, "garbage cursor"),
    )

    async def templates() -> None:
        r = await c.list_resource_templates()
        t = sorted(x.uri_template for x in r.resource_templates)
        expect(t == ["nugget://entity/{id}", "nugget://item/{source}/{key}"], f"templates {t}")

    await check("resources/templates/list", templates)

    async def read_entity() -> None:
        r = await c.read_resource(state["entity"]["uri"])
        first = r.contents[0]
        expect(isinstance(first, types.TextResourceContents), "text contents")
        expect(first.mime_type == "text/markdown", f"mime {first.mime_type}")
        expect("Compat Project 001" in first.text, "entity page")

    async def read_listed() -> None:
        uri = str(resources[-1].uri)
        r = await c.read_resource(uri)
        expect(str(r.contents[0].uri) == uri, "echoes the uri")

    async def read_item() -> None:
        r = await c.read_resource(state["hit"]["uri"])
        first = r.contents[0]
        expect(isinstance(first, types.TextResourceContents), "text contents")
        expect(first.mime_type == "text/plain", f"mime {first.mime_type}")
        expect("Falcon" in first.text, "item text")

    await check("resources/read entity", read_entity)
    await check("resources/read listed entity", read_listed)
    await check("resources/read item", read_item)
    # Resource not found: -32002 up to 2025-11-25, -32602 (invalid params) from 2026-07-28.
    not_found = INVALID_PARAMS if modern else RESOURCE_NOT_FOUND
    await check(
        f"resources/read unknown entity is {not_found}",
        lambda: raises(c.read_resource("nugget://entity/ent_does_not_exist"), not_found, "unknown entity"),
    )
    await check(
        f"resources/read foreign URI is {not_found}",
        lambda: raises(c.read_resource("file:///etc/passwd"), not_found, "foreign uri"),
    )

    async def cancellation() -> None:
        # Abandon in-flight calls (the SDK sends notifications/cancelled, or on the 2026 HTTP wire
        # closes the request's stream); the session must keep working.
        for i in range(3):
            with anyio.move_on_after(0.005):
                await call("get_context", {"query": f"Falcon launch {i}", "budget_tokens": 8000})
        if not (modern and kind == "http"):
            # A stray cancellation for an id that never existed is ignored.
            await c.session.send_notification(
                types.CancelledNotification(params=types.CancelledNotificationParams(request_id=987654, reason="stray"))
            )
        r = await call("search", {"query": "Falcon"})
        expect(not r.is_error, "the session still answers after cancellations")

    await check("cancellation", cancellation)


# ---- access errors ----


async def refused(kind: str, server: Any, revision: str) -> None:
    try:
        async with Client(server, mode=mode_for(revision), read_timeout_seconds=15) as c:
            await c.list_tools()
    except BaseException as e:  # noqa: BLE001 - the refusal may surface as any error (or a group)
        text = repr(e)
        if kind == "http":
            expect("bearer token" in text and "401" in text, f"want HTTP 401 with the reason, got {text[:300]}")
        return
    raise AssertionError("connected")


async def access(kind: str) -> None:
    global section
    section = f"[py {kind} access]"
    conn = take()
    rev = REVISIONS[-1]
    revoked = stdio(FX["revoked"]) if kind == "stdio" else http(FX["revoked"])
    await check("revoked client is refused", lambda: refused(kind, revoked, rev))
    wrong = (
        stdio(conn, {"NUGGET_MCP_TOKEN": "ngt_wrong"})
        if kind == "stdio"
        else http(conn, {"Authorization": "Bearer ngt_wrong"})
    )
    await check("wrong token is refused", lambda: refused(kind, wrong, rev))

    async def limited() -> None:
        async with client(kind, FX["limited"], rev) as c:
            r = await c.list_tools()
            expect([t.name for t in r.tools] == ["search"], f"tools {[t.name for t in r.tools]}")
            denied = await c.call_tool("get_entity", {"id": "x"})
            expect(denied.is_error is True and "not enabled" in text_of(denied), "get_entity is denied")
            res = await c.list_resources()
            expect(res.resources == [], "no entity resources without get_entity")

    await check("scoped client sees only its tools", limited)


async def rate_limit(kind: str) -> None:
    global section
    section = f"[py {kind} rate-limit]"
    conn = take()

    async def burst() -> None:
        async with client(kind, conn, REVISIONS[-1]) as c:
            errors: list[BaseException] = []

            async def one() -> None:
                try:
                    r = await c.call_tool("search", {"query": "Falcon"})
                    if r.is_error and "rate limited" in text_of(r):
                        errors.append(RuntimeError(text_of(r)))
                except Exception as e:  # noqa: BLE001
                    errors.append(e)

            async with anyio.create_task_group() as tg:
                for _ in range(80):
                    tg.start_soon(one)
            expect(bool(errors), "no request was rate limited")
            expect(any("rate limited" in str(e) or "429" in str(e) for e in errors), f"rate-limit error says so: {errors[0]}")

    await check("burst over the limit is refused", burst)


_next = 0


def take() -> dict[str, Any]:
    global _next
    conn = FX["clients"][_next]
    _next += 1
    return conn


async def main() -> int:
    print(f"Python SDK mcp {version('mcp')}; testing {', '.join(REVISIONS)}", flush=True)
    for kind in ("stdio", "http"):
        for revision in REVISIONS:
            await leg(kind, take(), revision)
        await access(kind)
        await rate_limit(kind)
    failed = [r for r in results if not r[1]]
    print(f"\nPython SDK: {len(results) - len(failed)} passed, {len(failed)} failed")
    for label, _, msg in failed:
        print(f"  FAIL {label}: {msg}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(anyio.run(main))
