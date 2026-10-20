"""Exercise the real loopback adapter with native Hermes task persistence."""

import unittest
import uuid

from aiohttp.test_utils import TestClient, TestServer
from echo_bridge.server import application
from hermes_cli import kanban_db as kb


class BridgeGate(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.board = "echo-http-" + uuid.uuid4().hex[:10]
        kb.create_board(self.board, name="Echo HTTP fixture")
        self.token = "synthetic-echo-test-token-not-a-secret-1234"
        self.client = TestClient(TestServer(application(self.token, 8642, self.board)))
        await self.client.start_server()
        self.addAsyncCleanup(self.client.close)

    async def test_auth_required_on_every_route(self):
        for path in ("/echo/capabilities", "/echo/tasks", "/echo/events", "/echo/runtime/idle", "/native/health"):
            self.assertEqual((await self.client.get(path)).status, 401)
        self.assertEqual((await self.client.post("/echo/codex/profiles", json={})).status, 401)

    async def test_codex_import_errors_do_not_expose_source_contents(self):
        from unittest.mock import patch
        headers = {"Authorization": "Bearer " + self.token}
        malformed = await self.client.post("/echo/codex/profiles", json=[], headers=headers)
        self.assertEqual(malformed.status, 400)
        with patch("echo_bridge.codex_import.dispatch", side_effect=OSError("private synthetic transcript")):
            response = await self.client.post("/echo/codex/profiles", json={}, headers=headers)
            self.assertEqual(response.status, 200)
            body = await response.json()
            self.assertIn("error", body)
            self.assertNotIn("private synthetic transcript", body["error"])

    async def test_reconnect_gate_checks_native_tasks_and_conversation_leases(self):
        from echo_bridge.native import board_connection
        from hermes_state import SessionDB
        headers = {"Authorization": "Bearer " + self.token}
        async def idle():
            result = await self.client.get("/echo/runtime/idle", headers=headers)
            self.assertEqual(result.status, 200)
            return (await result.json())["idle"]
        self.assertTrue(await idle())
        with board_connection(self.board) as conn:
            task = kb.create_task(conn, title="Pending reconnect fixture", initial_status="blocked")
        self.assertFalse(await idle())
        with board_connection(self.board) as conn:
            kb.archive_task(conn, task)
        db = SessionDB()
        self.addCleanup(db.close)
        session = "echo-idle-gate-" + uuid.uuid4().hex
        db.create_session(session, "echo")
        self.assertTrue(db.try_acquire_session_turn_lease(session, "echo-reconnect-fixture", ttl_seconds=60))
        self.assertFalse(await idle())
        db.release_session_turn_lease(session, "echo-reconnect-fixture")
        self.assertTrue(await idle())

    async def test_origin_rejected_even_with_token(self):
        result = await self.client.get("/echo/tasks", headers={
            "Authorization": "Bearer " + self.token, "Origin": "https://example.com",
        })
        self.assertEqual(result.status, 403)

    async def test_no_arbitrary_native_proxy(self):
        result = await self.client.get("/native/api/plugins/kanban/tasks", headers={
            "Authorization": "Bearer " + self.token,
        })
        self.assertEqual(result.status, 404)

    async def test_capabilities_identify_native_authority(self):
        result = await self.client.get("/echo/capabilities", headers={
            "Authorization": "Bearer " + self.token,
        })
        self.assertEqual(result.status, 200)
        self.assertEqual((await result.json())["task_authority"], "hermes-native")

    async def test_invalid_metadata_body_is_a_client_error(self):
        result = await self.client.post("/echo/history-stamps",json=[],headers={"Authorization":"Bearer "+self.token})
        self.assertEqual(result.status,400)

    async def test_worker_model_override_keeps_private_app_server_route(self):
        from echo_bridge.native import board_connection
        with board_connection(self.board) as conn:
            task = kb.create_task(conn,title="Worker model fixture",assignee="echo-codex",initial_status="blocked")
        headers = {"Authorization":"Bearer "+self.token}
        result = await self.client.post(f"/echo/tasks/{task}/model",json={"model":"gpt-5.4"},headers=headers)
        self.assertEqual(result.status,200)
        with board_connection(self.board) as conn:
            self.assertEqual(kb.get_task(conn,task).provider_override,"openai")
        result = await self.client.post(f"/echo/tasks/{task}/retry",json={},headers=headers)
        self.assertEqual(result.status,200)
        continuation = (await result.json())["task_id"]
        with board_connection(self.board) as conn:
            self.assertEqual(kb.get_task(conn,continuation).provider_override,"openai")

    async def test_search_and_streaming_export_use_native_session_source(self):
        from hermes_state import SessionDB
        db = SessionDB()
        self.addCleanup(db.close)
        session = "echo-http-history-" + uuid.uuid4().hex
        db.create_session(session,"echo")
        text = "漢字 " + "x" * 70000 + " unique fixture marker"
        db.append_message(session,"assistant",text)
        headers = {"Authorization":"Bearer "+self.token}
        search = await self.client.post(f"/echo/history/{session}/search",json={"text":"unique fixture marker"},headers=headers)
        self.assertEqual(search.status,200)
        self.assertEqual(len((await search.json())["results"]),1)
        export = await self.client.get(f"/echo/history/{session}/export",headers=headers)
        self.assertEqual(export.status,200)
        self.assertEqual(await export.text(),"\n\nassistant\n"+text)


if __name__ == "__main__":
    unittest.main(verbosity=2)
