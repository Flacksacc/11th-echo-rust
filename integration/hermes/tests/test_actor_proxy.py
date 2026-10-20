import asyncio
from pathlib import Path
import tempfile
import unittest

from echo_bridge.actor_proxy import actor_lock, forwarding_server


class ActorGate(unittest.IsolatedAsyncioTestCase):
    async def test_shared_actor_lock_serializes_and_releases_cancelled_waiters(self):
        with tempfile.TemporaryDirectory(prefix="echo-actor-gate-") as temporary:
            path = Path(temporary) / "actor.lock"
            entered = asyncio.Event()

            async def waiting():
                async with actor_lock(path):
                    entered.set()

            async with actor_lock(path):
                waiter = asyncio.create_task(waiting())
                await asyncio.sleep(0.075)
                self.assertFalse(entered.is_set())
                waiter.cancel()
                with self.assertRaises(asyncio.CancelledError):
                    await waiter
            await asyncio.wait_for(waiting(), 1)
            self.assertTrue(entered.is_set())

    async def test_native_mcp_schema_and_result_survive_forwarding(self):
        from mcp import types
        from mcp.shared.memory import create_client_server_memory_streams
        from mcp import ClientSession
        tool = types.Tool(name="inspect_fixture", description="Fixture", inputSchema={"type":"object", "properties":{"text":{"type":"string"}}},
                          annotations=types.ToolAnnotations(readOnlyHint=True))
        result = types.CallToolResult(content=[types.TextContent(type="text", text="native fixture result")],
                                      structuredContent={"checked":True}, isError=False)

        class NativeSession:
            async def list_tools(self, cursor=None):
                return types.ListToolsResult(tools=[tool])
            async def call_tool(self, name, arguments):
                self.called = (name,arguments)
                return result

        with tempfile.TemporaryDirectory(prefix="echo-actor-protocol-") as temporary:
            native = NativeSession()
            server = forwarding_server(native, Path(temporary)/"actor.lock")
            async with create_client_server_memory_streams() as (client_streams, server_streams):
                serving = asyncio.create_task(server.run(*server_streams, server.create_initialization_options()))
                try:
                    async with ClientSession(*client_streams) as client:
                        await client.initialize()
                        self.assertEqual((await client.list_tools()).tools, [tool])
                        self.assertEqual(await client.call_tool("inspect_fixture", {"text":"hello"}), result)
                        self.assertEqual(native.called, ("inspect_fixture", {"text":"hello"}))
                finally:
                    serving.cancel()
                    try:
                        await serving
                    except asyncio.CancelledError:
                        pass
