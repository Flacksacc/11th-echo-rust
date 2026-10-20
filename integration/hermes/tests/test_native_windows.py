"""Actual Windows-process gate; no model credentials or private transcripts.

Run inside Hermes' managed environment with integration/hermes on PYTHONPATH.
Uses an isolated native board. Synthetic workers deliberately spawn a child,
so cancellation must prove both processes exited, not just change task status.
"""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
import uuid

from echo_bridge.native import board_connection, event_page, queued_instruction, stop_task, task_conversation
from hermes_cli import kanban_db as kb
from hermes_cli.kanban_db_dispatch import _set_worker_pid


@unittest.skipUnless(sys.platform == "win32", "Native Windows gate")
class NativeTaskGate(unittest.TestCase):
    def setUp(self):
        self.board = "echo-gate-" + uuid.uuid4().hex[:10]
        kb.create_board(self.board, name="Echo synthetic gate")
        self.tmp = tempfile.TemporaryDirectory(prefix="echo-gate-")
        self.addCleanup(self.cleanup_temporary_directory)

    def cleanup_temporary_directory(self):
        # Windows can retain a just-closed SQLite/tracing file briefly. Retry
        # deletion of this test-owned directory; never suppress a lasting leak.
        for attempt in range(50):
            try:
                self.tmp.cleanup()
                return
            except PermissionError:
                if attempt == 49:
                    raise
                time.sleep(0.1)

    def create_task(self):
        with board_connection(self.board) as conn:
            return kb.create_task(conn, title="Synthetic Echo lifecycle test", assignee="default")

    def test_queued_comment_does_not_cancel_or_acknowledge(self):
        task_id = self.create_task()
        with board_connection(self.board) as conn:
            task = kb.claim_task(conn, task_id)
            self.assertIsNotNone(task)
        result = queued_instruction(task_id, "Check the second fixture too.", self.board)
        self.assertEqual(result["status"], "queued")
        with board_connection(self.board) as conn:
            self.assertEqual(kb.get_task(conn, task_id).status, "running")
            self.assertEqual(kb.list_comments(conn, task_id)[-1].body, "Check the second fixture too.")
        stop_task(task_id, self.board)

    def test_cursor_replay_is_bounded_and_deterministic(self):
        for _ in range(3):
            self.create_task()
        first = event_page(0, 2, self.board)
        self.assertEqual(len(first["events"]), 2)
        replay = event_page(0, 2, self.board)
        self.assertEqual(first, replay)
        second = event_page(first["next_cursor"], 2, self.board)
        self.assertTrue(all(e["id"] > first["next_cursor"] for e in second["events"]))

    def test_task_conversation_uses_idempotent_native_wake_subscription(self):
        task = self.create_task()
        session = task_conversation(task, self.board)
        self.assertEqual(task_conversation(task, self.board), session)
        with board_connection(self.board) as conn:
            rows = conn.execute("SELECT chat_id,delivery_mode,notifier_profile FROM kanban_notify_subs WHERE task_id=?",(task,)).fetchall()
        self.assertEqual(len(rows),1)
        self.assertEqual(tuple(rows[0]),(session,"wake","default"))
        from contextlib import closing
        from hermes_state import SessionDB
        with closing(SessionDB(read_only=True)) as db:
            self.assertEqual(db.get_session(session)["source"],"api_server")

    def test_comment_projection_preserves_native_order_and_bounds_text(self):
        task = self.create_task()
        with board_connection(self.board) as conn:
            kb.add_comment(conn, task, "worker", "first progress")
            kb.add_comment(conn, task, "worker", "second progress " + "x" * 10000)
        comments = [e for e in event_page(0, 100, self.board)["events"] if e["kind"] == "commented"]
        self.assertEqual(comments[0]["payload"]["body"], "first progress")
        self.assertTrue(comments[1]["payload"]["body"].startswith("second progress"))
        self.assertLessEqual(len(comments[1]["payload"]["body"]), 4096)

    @unittest.skipUnless(os.environ.get("ECHO_GATE_CODEX_BIN"), "Set ECHO_GATE_CODEX_BIN for actual Codex protocol gate")
    def test_native_codex_transport_initializes_with_private_home(self):
        from agent.transports.codex_app_server import CodexAppServerClient
        codex_home = Path(self.tmp.name) / "codex"
        codex_home.mkdir()
        client = CodexAppServerClient(
            codex_bin=os.environ["ECHO_GATE_CODEX_BIN"], codex_home=str(codex_home),
        )
        self.addCleanup(client.close)
        client.initialize(timeout=15)
        self.assertTrue(client.is_alive())
        account = client.request("account/read", {"refreshToken": False}, timeout=15)
        self.assertIsNone(account.get("account"), "Fresh Echo CODEX_HOME inherited console login")
        client.close()
        self.assertFalse(client.is_alive(), "Native Codex app-server survived transport close")
        client._reader.join(timeout=2)
        client._stderr_reader.join(timeout=2)
        for pipe in (client._proc.stdout, client._proc.stderr):
            if pipe:
                pipe.close()

    @unittest.skipUnless(os.environ.get("ECHO_GATE_CODEX_BIN"), "Set ECHO_GATE_CODEX_BIN for actual Codex permission gate")
    def test_actual_codex_permission_profiles_override_native_worker_defaults(self):
        from agent.transports.codex_app_server import CodexAppServerClient
        codex_home = Path(self.tmp.name) / "permission-codex"
        codex_home.mkdir()
        for profile, expected in ((":read-only","readOnly"),(":workspace","workspaceWrite")):
            client = CodexAppServerClient(codex_bin=os.environ["ECHO_GATE_CODEX_BIN"], codex_home=str(codex_home),
                extra_args=["-c",'sandbox_mode="workspace-write"', "-c",f'default_permissions="{profile}"',
                    "-c","sandbox_workspace_write.writable_roots=[]", "-c","sandbox_workspace_write.network_access=false"])
            try:
                client.initialize(timeout=15)
                thread = client.request("thread/start",{"cwd":self.tmp.name,"ephemeral":True},timeout=15)
                self.assertEqual(thread["sandbox"]["type"],expected)
                self.assertFalse(thread["sandbox"].get("networkAccess",False))
            finally:
                client.close()
                client._reader.join(timeout=2); client._stderr_reader.join(timeout=2)
                for pipe in (client._proc.stdout,client._proc.stderr):
                    if pipe: pipe.close()

    def test_archive_terminates_worker_and_descendant_and_prevents_redispatch(self):
        task_id = self.create_task()
        pid_file = Path(self.tmp.name) / "child.pid"
        child_code = "import time; time.sleep(120)"
        worker_code = (
            "import subprocess,sys,time; from pathlib import Path; "
            "p=subprocess.Popen([sys.executable,'-c',sys.argv[2]],creationflags=subprocess.CREATE_NO_WINDOW); "
            "Path(sys.argv[1]).write_text(str(p.pid)); time.sleep(120)"
        )
        worker = subprocess.Popen(
            [sys.executable, "-c", worker_code, str(pid_file), child_code],
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            creationflags=subprocess.CREATE_NO_WINDOW,
        )
        from agent.deadline import kill_process_tree
        self.addCleanup(lambda: kill_process_tree(worker.pid) if worker.poll() is None else None)
        deadline = time.monotonic() + 10
        while not pid_file.exists() and time.monotonic() < deadline:
            time.sleep(0.02)
        self.assertTrue(pid_file.exists(), "Worker did not start its descendant")
        child_pid = int(pid_file.read_text())
        self.addCleanup(lambda: kill_process_tree(child_pid) if kb._pid_alive(child_pid) else None)
        with board_connection(self.board) as conn:
            self.assertIsNotNone(kb.claim_task(conn, task_id))
            _set_worker_pid(conn, task_id, worker.pid)
        result = stop_task(task_id, self.board)
        worker.wait(timeout=10)
        self.assertFalse(kb._pid_alive(child_pid), "Codex-style descendant survived Stop Task")
        self.assertTrue(result["stopped"], result)
        with board_connection(self.board) as conn:
            self.assertEqual(kb.get_task(conn, task_id).status, "archived")
            self.assertIsNone(kb.claim_task(conn, task_id), "Stopped task became dispatchable")


if __name__ == "__main__":
    unittest.main(verbosity=2)
