"""Native HTTP sessions/runs/SSE tested without sending a paid model request.

Only the provider boundary is a scripted fixture. Session persistence, auth,
run admission, concurrency, callbacks, events and stop use upstream Hermes.
An authenticated Codex end-to-end run is a separate setup-time readiness gate.
"""

import asyncio
import json
import threading
import unittest
import uuid
from unittest.mock import MagicMock, patch

from aiohttp import web
from aiohttp.test_utils import TestClient, TestServer
from gateway.config import PlatformConfig
from gateway.platforms.api_server import APIServerAdapter


class NativeApiGate(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.token = "synthetic-native-api-test-token-1234567890"
        self.adapter = APIServerAdapter(PlatformConfig(enabled=True, extra={"key": self.token}))
        self.app = web.Application()
        for method, path, handler in self.adapter._http_route_table():
            self.app.router.add_route(method, path, handler)
        self.client = TestClient(TestServer(self.app))
        await self.client.start_server()
        self.addAsyncCleanup(self.client.close)
        from gateway.platforms.api_server_runs import _close_run_state
        self.addCleanup(_close_run_state, self.adapter)
        self.headers = {"Authorization": "Bearer " + self.token}

    async def asyncTearDown(self):
        db = getattr(self.adapter, "_session_db", None)
        if db is not None:
            db.close()

    async def test_session_creation_and_bounded_pagination(self):
        response = await self.client.post("/api/sessions", json={"title": "Echo native gate " + uuid.uuid4().hex[:10]}, headers=self.headers)
        self.assertEqual(response.status, 201, await response.text())
        session = (await response.json())["session"]["id"]
        response = await self.client.get(
            f"/api/sessions/{session}/messages?limit=3&order=latest", headers=self.headers,
        )
        self.assertEqual(response.status, 200, await response.text())
        self.assertEqual((await response.json())["pagination"]["limit"], 3)

    async def test_native_runs_require_auth(self):
        response = await self.client.post("/v1/runs", json={"input": "Synthetic fixture"})
        self.assertEqual(response.status, 401)

    async def test_stopping_one_run_leaves_other_run_working(self):
        ready = {name: threading.Event() for name in ("one", "two")}
        released = {name: threading.Event() for name in ready}
        for event in released.values():
            self.addCleanup(event.set)

        def factory(**kwargs):
            agent = MagicMock()
            agent.session_prompt_tokens = agent.session_completion_tokens = agent.session_total_tokens = 0
            def run(**args):
                name = args["user_message"]
                ready[name].set()
                released[name].wait(10)
                return {"final_response": "Stopped fixture."}
            agent.run_conversation.side_effect = run
            agent.interrupt.side_effect = lambda *_: released[agent.run_conversation.call_args.kwargs["user_message"]].set()
            return agent

        with patch.object(self.adapter, "_create_agent", side_effect=factory):
            runs = {}
            for name in ready:
                response = await self.client.post("/v1/runs", json={"input": name}, headers=self.headers)
                self.assertEqual(response.status, 202, await response.text())
                runs[name] = (await response.json())["run_id"]
                self.assertTrue(await asyncio.to_thread(ready[name].wait, 5))
            response = await self.client.post(f"/v1/runs/{runs['one']}/stop", headers=self.headers)
            self.assertEqual(response.status, 200, await response.text())
            self.assertEqual((await response.json())["status"], "stopping")
            self.assertTrue(await asyncio.to_thread(released["one"].wait, 5))
            self.assertFalse(released["two"].is_set(), "Stopping one run interrupted another conversation")
            response = await self.client.post(f"/v1/runs/{runs['two']}/stop", headers=self.headers)
            self.assertEqual(response.status, 200, await response.text())
            self.assertTrue(await asyncio.to_thread(released["two"].wait, 5))
            for run in runs.values():
                response = await self.client.get(f"/v1/runs/{run}/events", headers=self.headers)
                await asyncio.wait_for(response.text(), timeout=5)

    async def test_parallel_fast_session_does_not_wait_for_worker(self):
        ready = threading.Event()
        release = threading.Event()
        self.addCleanup(release.set)

        def factory(**kwargs):
            agent = MagicMock()
            agent.session_prompt_tokens = agent.session_completion_tokens = agent.session_total_tokens = 0
            def run(**args):
                if args["user_message"] == "long task":
                    ready.set()
                    release.wait(10)
                kwargs["interim_assistant_callback"]("Checking the fixture.", already_streamed=False)
                kwargs["stream_delta_callback"]("Done.")
                return {"final_response": "Done."}
            agent.run_conversation.side_effect = run
            agent.interrupt.side_effect = lambda *_: release.set()
            return agent

        with patch.object(self.adapter, "_create_agent", side_effect=factory):
            response = await self.client.post("/v1/runs", json={"input": "long task"}, headers=self.headers)
            self.assertEqual(response.status, 202, await response.text())
            worker = (await response.json())["run_id"]
            self.assertTrue(await asyncio.to_thread(ready.wait, 5))
            response = await self.client.post("/v1/runs", json={"input": "quick question"}, headers=self.headers)
            self.assertEqual(response.status, 202, await response.text())
            fast = (await response.json())["run_id"]
            response = await self.client.get(f"/v1/runs/{fast}/events", headers=self.headers)
            body = await asyncio.wait_for(response.text(), timeout=5)
            events = [json.loads(line[6:]) for line in body.splitlines() if line.startswith("data: ")]
            self.assertIn("message.interim", [e["event"] for e in events])
            self.assertIn("message.delta", [e["event"] for e in events])
            self.assertEqual(events[-1]["event"], "run.completed")
            self.assertFalse(release.is_set(), "Fast conversation interrupted the worker")
            response = await self.client.post(f"/v1/runs/{worker}/stop", headers=self.headers)
            self.assertEqual(response.status, 200, await response.text())
            self.assertTrue(await asyncio.to_thread(release.wait, 5))
            # Drain the native run so no test executor leaks past teardown.
            response = await self.client.get(f"/v1/runs/{worker}/events", headers=self.headers)
            await asyncio.wait_for(response.text(), timeout=5)


if __name__ == "__main__":
    unittest.main(verbosity=2)
