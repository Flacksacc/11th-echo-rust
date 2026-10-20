"""Serialize managed Windows/browser MCP calls across native Hermes sessions.

This is a transport adapter: schemas, results and execution stay with the
installed MCP server. Hermes still owns tool approval and agent execution.
"""
from __future__ import annotations

import asyncio
import errno
from contextlib import asynccontextmanager
import json
import os
from pathlib import Path
import sys


@asynccontextmanager
async def actor_lock(path: Path):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a+b") as stream:
        if stream.tell() == 0:
            stream.write(b"\0")
            stream.flush()
        acquired = False
        try:
            while not acquired:
                stream.seek(0)
                try:
                    if sys.platform == "win32":
                        import msvcrt
                        msvcrt.locking(stream.fileno(), msvcrt.LK_NBLCK, 1)
                    else:
                        import fcntl
                        fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    acquired = True
                except OSError as error:
                    # Permanent filesystem/descriptor errors must surface;
                    # retry only another actor's lock ownership.
                    if error.errno not in {errno.EACCES, errno.EAGAIN, errno.EDEADLK}:
                        raise
                    await asyncio.sleep(0.025)
            yield
        finally:
            if acquired:
                stream.seek(0)
                if sys.platform == "win32":
                    import msvcrt
                    msvcrt.locking(stream.fileno(), msvcrt.LK_UNLCK, 1)
                else:
                    import fcntl
                    fcntl.flock(stream, fcntl.LOCK_UN)


def entry_for(index: str):
    if not index.isdecimal() or len(index) > 6:
        raise ValueError("Invalid managed actor index")
    root = Path(os.environ["HERMES_HOME"]).parent
    path = root / "echo-config.json"
    if path.stat().st_size > 1024 * 1024:
        raise ValueError("Managed actor configuration exceeded its budget")
    from hermes_cli.config import _expand_env_vars
    supplied = json.loads(path.read_text(encoding="utf-8"))
    entry = _expand_env_vars(supplied.get("echo_managed_actors", {}).get(index))
    if not isinstance(entry, dict) or not entry.get("command"):
        raise ValueError("Managed actor configuration is unavailable")
    return root, entry


def forwarding_server(session, lock_path):
    from mcp.server.lowlevel import Server

    async def tools(context, params):
        return await session.list_tools(cursor=params.cursor if params else None)

    async def call(context, params):
        async with actor_lock(lock_path):
            pending = asyncio.create_task(session.call_tool(params.name, params.arguments or {}))
            try:
                return await asyncio.shield(pending)
            except asyncio.CancelledError:
                # A cancelled conversation is not proof that a physical click
                # stopped. Hold ownership until the native tool call finishes.
                try:
                    await asyncio.shield(pending)
                finally:
                    raise

    return Server("echo-managed-actor", version="1", on_list_tools=tools, on_call_tool=call)


async def main(index):
    from mcp import ClientSession, StdioServerParameters
    from mcp.client.stdio import stdio_client
    from mcp.server.stdio import stdio_server
    root, entry = entry_for(index)
    parameters = StdioServerParameters(command=entry["command"], args=entry.get("args", []),
                                       env={**os.environ, **entry.get("env", {})}, cwd=entry.get("cwd"))
    # Child diagnostics can include tool arguments; do not forward them to Echo
    # logs. stdout is exclusively the MCP protocol.
    with open(os.devnull, "w") as diagnostics:
        async with stdio_client(parameters, errlog=diagnostics) as (read, write):
            async with ClientSession(read, write) as session:
                await session.initialize()
                server = forwarding_server(session, root / "actor.lock")
                async with stdio_server() as (incoming, outgoing):
                    await server.run(incoming, outgoing, server.create_initialization_options())


if __name__ == "__main__":
    asyncio.run(main(sys.argv[1]))
