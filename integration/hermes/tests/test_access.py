from pathlib import Path
import tempfile
import subprocess
import unittest
import uuid
from unittest.mock import patch
from hermes_cli import kanban_db as kb
from hermes_cli.kanban_db_workspace import resolve_workspace, set_workspace_path
from echo_bridge.native import board_connection, stop_task
from echo_bridge.access import approve_workspace, worker_permissions, retry_task


class AccessGate(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="echo-access-gate-")
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.workspace = self.directory / "project"
        self.workspace.mkdir()
        self.board = "echo-access-" + uuid.uuid4().hex[:12]
        kb.create_board(self.board)
        self.patch = patch("echo_bridge.access.grants_path", return_value=self.directory / "grants.json")
        self.patch.start()
        self.addCleanup(self.patch.stop)

    def task(self):
        with board_connection(self.board) as conn:
            return kb.create_task(conn,title="Modify fixture",assignee="echo-codex",initial_status="blocked")

    def test_workspace_approval_is_a_native_continuation_and_revokes_on_stop(self):
        task = self.task()
        result = approve_workspace(task,str(self.workspace),self.board)
        next_id = result["task_id"]
        with board_connection(self.board) as conn:
            self.assertEqual(kb.get_task(conn,task).status,"archived")
            self.assertEqual(kb.get_task(conn,next_id).status,"ready")
            kb.claim_task(conn,next_id)
        policy = worker_permissions(next_id,self.board)
        self.assertEqual(policy["profile"],":workspace")
        self.assertEqual(policy["workspace"],str(self.workspace.resolve()))
        self.assertFalse(policy["network"])
        stop_task(next_id,self.board)
        self.assertEqual(worker_permissions(next_id,self.board)["profile"],":read-only")

    def test_shared_folder_refuses_a_second_writer(self):
        approve_workspace(self.task(),str(self.workspace),self.board)
        second = self.task()
        with self.assertRaises(ValueError):
            approve_workspace(second,str(self.workspace),self.board)
        with board_connection(self.board) as conn:
            self.assertEqual(kb.get_task(conn,second).status,"blocked")

    def test_git_approvals_materialize_distinct_native_worktrees(self):
        def git(*args):
            subprocess.run(["git", "-C", str(self.workspace), *args], check=True,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        git("init")
        git("-c", "user.name=Echo fixture", "-c", "user.email=fixture@example.invalid", "commit", "--allow-empty", "-m", "fixture")
        ids = [approve_workspace(self.task(),str(self.workspace),self.board)["task_id"] for _ in range(2)]
        paths = []
        with board_connection(self.board) as conn:
            for task_id in ids:
                task = kb.claim_task(conn,task_id)
                workspace = resolve_workspace(task,board=self.board)
                set_workspace_path(conn,task_id,workspace)
                paths.append(workspace.resolve())
        self.assertNotEqual(*paths)
        self.assertTrue(all(path.is_relative_to(self.workspace / ".worktrees") for path in paths))
        for task_id, path in zip(ids, paths):
            self.assertEqual(Path(worker_permissions(task_id,self.board)["workspace"]),path)
            stop_task(task_id,self.board)

    def test_system_roots_and_unapproved_tasks_stay_read_only(self):
        task = self.task()
        self.assertEqual(worker_permissions(task,self.board)["profile"],":read-only")
        for folder in (Path(self.workspace.anchor),Path.home()):
            with self.assertRaises(ValueError):
                approve_workspace(task,str(folder),self.board)

    def test_retry_preserves_native_conversation_and_is_idempotent(self):
        from echo_bridge.native import task_conversation
        task = self.task()
        session = task_conversation(task,self.board)
        stop_task(task,self.board)
        reply = retry_task(task,self.board)
        self.assertEqual(reply["status"],"ready")
        self.assertEqual(task_conversation(reply["task_id"],self.board),session)
        self.assertEqual(retry_task(task,self.board)["task_id"],reply["task_id"])
        self.assertEqual(worker_permissions(reply["task_id"],self.board)["profile"],":read-only")

    def test_retry_does_not_reuse_workspace_grants(self):
        approved = approve_workspace(self.task(),str(self.workspace),self.board)["task_id"]
        stop_task(approved,self.board)
        reply = retry_task(approved,self.board)
        self.assertEqual(reply["status"],"blocked")
        self.assertEqual(worker_permissions(reply["task_id"],self.board)["profile"],":read-only")
        next_id = approve_workspace(reply["task_id"],str(self.workspace),self.board)["task_id"]
        with board_connection(self.board) as conn:
            kb.claim_task(conn,next_id)
        self.assertEqual(worker_permissions(next_id,self.board)["profile"],":workspace")

    def test_retry_refuses_active_or_unconfirmed_termination(self):
        task = self.task()
        with board_connection(self.board) as conn:
            kb.unblock_task(conn,task)
        with self.assertRaises(ValueError):
            retry_task(task,self.board)
        stop_task(task,self.board)
        with board_connection(self.board) as conn:
            kb._append_event(conn,task,"archive_worker_termination",{"terminated":False})
            conn.commit()
        with self.assertRaises(ValueError):
            retry_task(task,self.board)
