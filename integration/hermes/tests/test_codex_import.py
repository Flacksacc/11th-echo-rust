"""Synthetic profile fixtures; no model calls or real user history."""
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import tempfile
import time
import unittest
from contextlib import closing
from unittest.mock import patch

from echo_bridge import codex_import as importer


class ImportTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="echo-codex-test-")
        self.root = Path(self.temp.name)
        self.env = patch.dict(os.environ, {"HERMES_HOME": str(self.root / "hermes")})
        self.env.start()
        self.profile = self.root / "codex"
        (self.profile / "sessions").mkdir(parents=True)
        self.profiles = patch.object(importer, "profiles", return_value={"profiles": [{"id": "test", "name": "Test", "path": str(self.profile)}]})
        self.profiles.start()
        self.path = self.profile / "sessions/rollout.jsonl"
        self.args = {"profile": "test", "id": "thread-test", "path": str(self.path), "title": "Synthetic conversation", "cwd": str(self.root)}

    def tearDown(self):
        self.profiles.stop()
        self.env.stop()
        self.temp.cleanup()

    def write(self, extra):
        rows = [{"type": "session_meta", "payload": {"id": "thread-test", "cwd": str(self.root)}}] + extra
        self.path.write_text("".join(json.dumps(r, ensure_ascii=False) + "\n" for r in rows), encoding="utf-8")

    def message(self, role, text):
        return {"type": "response_item", "payload": {"type": "message", "role": role, "content": [{"type": "input_text" if role == "user" else "output_text", "text": text}]}}

    def wait(self, job):
        for _ in range(400):
            result = importer.job_status(job["job"])
            if result["status"] != "running":
                return result
            time.sleep(.025)
        self.fail("Import did not complete")

    def test_normalization_deduplication_and_partial_tail(self):
        self.write([self.message("user", "Repeat 界"), {"type": "event_msg", "payload": {"type": "user_message", "message": "Repeat 界"}}, self.message("assistant", "Answer"), self.message("user", "Repeat 界"), {"type": "response_item", "payload": {"type": "reasoning", "encrypted_content": "never-copy"}}, {"type": "response_item", "payload": {"type": "message", "role": "developer", "content": [{"type": "input_text", "text": "Old policy"}]}}])
        with self.path.open("ab") as stream:
            stream.write(b'{"unfinished":')
        rows = list(importer.normalized(self.path))
        self.assertEqual([r["content"] for r in rows], ["Repeat 界", "Answer", "Repeat 界"])

    def test_import_preserves_source_and_hidden_context(self):
        self.write([self.message("user", "What was the result?"), {"type": "response_item", "payload": {"type": "function_call_output", "call_id": "call-one", "output": "Synthetic useful tool result"}}, self.message("assistant", "It passed.")])
        before = hashlib.sha256(self.path.read_bytes()).hexdigest()
        result = self.wait(importer.start_import(self.args))
        self.assertEqual(result["status"], "complete", result)
        self.assertEqual(before, hashlib.sha256(self.path.read_bytes()).hexdigest())
        from hermes_state import SessionDB
        with SessionDB() as db:
            messages = db.get_messages_as_conversation(result["session"])
            self.assertTrue(any("Synthetic useful tool result" in str(m.get("content")) for m in messages))
            self.assertFalse(any(m.get("tool_calls") for m in messages))
            self.assertTrue(any(m["role"] == "assistant" and m["content"] == "It passed." for m in messages))
        from echo_bridge.history import history_page
        display = history_page(result["session"])
        self.assertTrue(any("What was the result?" in b["text"] for b in display["blocks"]))
        self.assertFalse(any("Synthetic useful tool result" in b["text"] for b in display["blocks"]))
        self.path.unlink()
        self.assertTrue((importer.storage() / result["job"] / "snapshot.jsonl").exists())
        self.assertTrue(history_page(result["session"])["blocks"])

    def test_catalog_pagination_and_archived(self):
        with closing(sqlite3.connect(self.profile / "state_5.sqlite")) as db:
            db.execute("CREATE TABLE threads(id TEXT,title TEXT,rollout_path TEXT,updated_at INTEGER,archived INTEGER,cwd TEXT)")
            db.executemany("INSERT INTO threads VALUES(?,?,?,?,?,?)", [(str(i), "Synthetic " + str(i), str(self.path), i, int(i == 99), str(self.root)) for i in range(100)])
            db.commit()
        args = {"profile": "test"}
        self.assertEqual(len(importer.catalog(args)["conversations"]), 40)
        self.assertTrue(importer.catalog(args)["more"])
        self.assertEqual(len(importer.catalog({**args, "offset": 80})["conversations"]), 19)
        self.assertEqual(len(importer.catalog({**args, "archived": True})["conversations"]), 1)

    def test_cancel_removes_only_the_import(self):
        self.write([self.message("user", "Question")])
        identity = "1" * 32
        importer._jobs[identity] = {"job": identity, "status": "running", "records": 0, "cancel": __import__("threading").Event()}
        importer._jobs[identity]["cancel"].set()
        importer.run_import(identity, self.path, self.args)
        self.assertEqual(importer.job_status(identity)["status"], "cancelled")
        self.assertFalse((importer.storage() / identity).exists())
        self.assertTrue(self.path.exists())

    def test_path_escape_is_rejected(self):
        self.write([])
        with self.assertRaises(ValueError):
            importer.selected({**self.args, "path": str(self.root / "outside.jsonl")})

    def test_event_only_legacy_messages(self):
        self.write([{"type": "event_msg", "payload": {"type": "user_message", "message": "Legacy question"}}, {"type": "event_msg", "payload": {"type": "agent_message", "message": "Legacy answer"}}])
        self.assertEqual([r["role"] for r in importer.normalized(self.path)], ["user", "assistant"])

    def test_repeated_imports_have_distinct_native_identities(self):
        self.write([self.message("user", "Question"), self.message("assistant", "Answer")])
        first = self.wait(importer.start_import(self.args))
        second = self.wait(importer.start_import(self.args))
        self.assertEqual(first["status"], "complete")
        self.assertEqual(second["status"], "complete")
        self.assertNotEqual(first["session"], second["session"])

    def test_malformed_import_rolls_back_without_changing_source(self):
        self.write([self.message("user", "Question")])
        with self.path.open("ab") as stream:
            stream.write(b'{bad record}\n')
        result = self.wait(importer.start_import(self.args))
        self.assertEqual(result["status"], "failed")
        self.assertTrue(self.path.exists())
        self.assertFalse((importer.storage() / result["job"]).exists())

    def test_available_images_are_copied_and_missing_images_are_recorded(self):
        asset = self.root / "avatar.png"
        asset.write_bytes(b"synthetic image fixture")
        self.write([self.message("user", "Image context"), {"type": "event_msg", "payload": {"type": "user_message", "message": "Image context", "local_images": [str(asset), str(self.root / "missing.png")]}}])
        result = self.wait(importer.start_import(self.args))
        self.assertEqual(result["status"], "complete", result)
        manifest = json.loads((importer.storage() / result["job"] / "attachments.json").read_text())
        self.assertEqual(Path(manifest[str(asset)]).read_bytes(), b"synthetic image fixture")
        self.assertIsNone(manifest[str(self.root / "missing.png")])

    def test_unsupported_database_uses_rollout_fallback(self):
        self.write([self.message("user", "Question")])
        with closing(sqlite3.connect(self.profile / "state_99.sqlite")) as db:
            db.execute("CREATE TABLE unknown_schema(x TEXT)")
        rows = importer.catalog({"profile": "test"})["conversations"]
        self.assertEqual(rows[0]["id"], "thread-test")

    def test_native_compaction_keeps_imported_history_browsable(self):
        self.write([self.message("user", "Remember cobalt."), self.message("assistant", "The project detail is cobalt.")])
        result = self.wait(importer.start_import(self.args))
        self.assertEqual(result["status"], "complete")
        from hermes_state import SessionDB
        with SessionDB() as db:
            db.archive_and_compact(result["session"], [{"role": "user", "content": "Native compacted context: remember cobalt.", "_compressed_summary": True}])
            context = db.get_messages_as_conversation(result["session"])
            self.assertTrue(any("Native compacted context" in str(m.get("content")) for m in context))
            db.append_message(result["session"], "user", "Continue this project.")
            self.assertTrue(any(m["content"] == "Continue this project." for m in db.get_messages_as_conversation(result["session"])))
        from echo_bridge.history import history_page
        self.assertTrue(any("The project detail is cobalt." in b["text"] for b in history_page(result["session"])["blocks"]))

    def test_cancel_after_first_batch_preserves_unrelated_session(self):
        self.write([self.message("user" if i % 2 == 0 else "assistant", "Synthetic " + str(i)) for i in range(100)])
        from hermes_state import SessionDB
        with SessionDB() as db:
            db.ensure_session("unrelated", "api_server")
            db.append_message("unrelated", "user", "Keep this message")
        original = SessionDB.append_messages_batch
        def cancel_after_batch(db, session, rows, **kwargs):
            result = original(db, session, rows, **kwargs)
            importer._jobs[session.removeprefix("api_echo_import_")]["cancel"].set()
            return result
        with patch.object(SessionDB, "append_messages_batch", cancel_after_batch):
            result = self.wait(importer.start_import(self.args))
        self.assertEqual(result["status"], "cancelled")
        with SessionDB() as db:
            self.assertIsNone(db.get_session("api_echo_import_" + result["job"]))
            self.assertEqual(db.get_messages_as_conversation("unrelated")[0]["content"], "Keep this message")

    def test_large_preview_has_bounded_page_memory(self):
        import tracemalloc
        self.write([])
        message = self.message("user", "Synthetic paragraph " * 210)
        message["payload"]["id"] = "identity-placeholder"
        record = json.dumps(message) + "\n"
        with self.path.open("a", encoding="utf-8") as stream:
            for index in range(26000):
                stream.write(record.replace("identity-placeholder", "identity-" + str(index)))
        self.assertGreater(self.path.stat().st_size, 100 * 1024 * 1024)
        tracemalloc.start()
        page = importer.preview({**self.args, "offset": 50000})
        _, peak = tracemalloc.get_traced_memory()
        tracemalloc.stop()
        self.assertEqual(len(page["blocks"]), 20)
        self.assertTrue(page["more"])
        self.assertLess(peak, 4 * 1024 * 1024)

    def test_storage_failure_finishes_job_instead_of_leaving_it_running(self):
        self.write([self.message("user", "Question")])
        with patch.object(importer, "storage", side_effect=OSError("synthetic storage failure")):
            result = self.wait(importer.start_import(self.args))
        self.assertEqual(result["status"], "failed")
        self.assertNotIn("synthetic storage failure", result["error"])

    def test_embedded_images_survive_without_network_requests(self):
        import base64
        image = "data:image/png;base64," + base64.b64encode(b"synthetic embedded image").decode()
        row = self.message("user", "See this image")
        row["payload"]["content"].extend([{"type": "input_image", "image_url": image}, {"type": "input_image", "image_url": "https://example.invalid/image.png"}])
        self.write([row])
        result = self.wait(importer.start_import(self.args))
        self.assertEqual(result["status"], "complete")
        manifest = json.loads((importer.storage() / result["job"] / "attachments.json").read_text())
        self.assertIn(b"synthetic embedded image", [Path(path).read_bytes() for path in manifest.values() if path])
        self.assertIsNone(manifest["https://example.invalid/image.png"])
        from echo_bridge.history import history_page
        self.assertTrue(any("unavailable" in b["text"] for b in history_page(result["session"])["blocks"]))

    def test_recovery_removes_only_incomplete_owned_imports(self):
        from hermes_state import SessionDB
        incomplete = importer.storage() / ("a" * 32)
        completed = importer.storage() / ("b" * 32)
        unrelated = importer.storage() / "user-files"
        for folder in (incomplete, completed, unrelated):
            folder.mkdir()
        (completed / "complete.json").write_text("{}")
        with SessionDB() as db:
            for identity in ("a", "b"):
                db.ensure_session("api_echo_import_" + identity * 32, "api_server")
        with patch.object(importer, "_recovered", False):
            importer.recover()
        self.assertFalse(incomplete.exists())
        self.assertTrue(completed.exists())
        self.assertTrue(unrelated.exists())
        with SessionDB() as db:
            self.assertIsNone(db.get_session("api_echo_import_" + "a" * 32))
            self.assertIsNotNone(db.get_session("api_echo_import_" + "b" * 32))

    def test_custom_archived_only_profile_is_discoverable(self):
        archived = self.root / "archived-profile"
        (archived / "archived_sessions").mkdir(parents=True)
        (importer.storage() / "profiles.json").write_text(json.dumps([str(archived), str(archived)]))
        original = self.profiles.temp_original
        with patch.object(importer, "recover"), patch.object(Path, "home", return_value=self.root), patch.dict(os.environ, {"APPDATA": str(self.root), "CODEX_HOME": str(archived)}):
            result = original()
        self.assertEqual(len(result["profiles"]), 1)
        self.assertEqual(result["profiles"][0]["path"], str(archived.resolve()))

    def test_native_run_receives_imported_context(self):
        import asyncio
        from unittest.mock import MagicMock
        from aiohttp import web
        from aiohttp.test_utils import TestClient, TestServer
        from gateway.config import PlatformConfig
        from gateway.platforms.api_server import APIServerAdapter
        from gateway.platforms.api_server_runs import _close_run_state
        self.write([self.message("user", "Remember cobalt."), self.message("assistant", "The project is cobalt.")])
        imported = self.wait(importer.start_import(self.args))
        self.assertEqual(imported["status"], "complete")
        received = []
        token = "synthetic-import-continuation-test-token-123456789"
        adapter = APIServerAdapter(PlatformConfig(enabled=True, extra={"key": token}))
        def factory(**kwargs):
            agent = MagicMock()
            agent.session_id = imported["session"]
            agent.session_prompt_tokens = agent.session_completion_tokens = agent.session_total_tokens = 0
            def run(**args):
                received.extend(args["conversation_history"])
                return {"final_response": "Synthetic continuation."}
            agent.run_conversation.side_effect = run
            return agent
        async def exercise():
            app = web.Application()
            for method, path, handler in adapter._http_route_table():
                app.router.add_route(method, path, handler)
            async with TestClient(TestServer(app)) as client:
                headers = {"Authorization": "Bearer " + token}
                response = await client.post("/v1/runs", json={"input": "Continue this project.", "session_id": imported["session"]}, headers=headers)
                self.assertEqual(response.status, 202, await response.text())
                run = (await response.json())["run_id"]
                events = await client.get(f"/v1/runs/{run}/events", headers=headers)
                body = await asyncio.wait_for(events.text(), timeout=5)
                self.assertIn("run.completed", body)
        try:
            with patch.object(adapter, "_create_agent", side_effect=factory):
                asyncio.run(exercise())
            self.assertTrue(any(row.get("content") == "The project is cobalt." for row in received))
        finally:
            _close_run_state(adapter)
            adapter._close_cached_session_dbs()
            adapter._response_store.close()


if __name__ == "__main__":
    unittest.main()
