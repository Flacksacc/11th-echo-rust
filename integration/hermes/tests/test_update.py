"""Real filesystem transaction/recovery tests; updater boundary is scripted."""
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import types
import unittest
from contextlib import closing
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("echo_update", Path(__file__).parents[1] / "echo_bridge/update.py")
update = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(update)


class UpdateTransaction(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="echo-update-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.source = self.root / "source"
        self.source.mkdir()
        self.home = self.root / "home"
        self.home.mkdir()
        self.old = self.home / "installs/test/environments/old/venv/Scripts/python.exe"
        self.old.parent.mkdir(parents=True)
        self.old.write_bytes(b"synthetic interpreter")
        self.receipt = self.home / "installs/test/facts.json"
        self.select(self.old)
        self.source.joinpath("runtime.txt").write_text("old version")
        self.home.joinpath("config.yaml").write_text("private synthetic config")
        self.root.joinpath("codex-home").mkdir()
        self.root.joinpath("codex-home/auth.json").write_text("synthetic account marker")
        self.lease = types.ModuleType("hermes_cli.runtime_state")
        self.lease.lease_generation = lambda _: lambda: None
        self.stub = patch.dict("sys.modules", {"hermes_cli.runtime_state": self.lease})
        self.stub.start()
        self.addCleanup(self.stub.stop)
        subprocess.run(["git", "init", "-q", str(self.source)], check=True)
        subprocess.run(["git", "-C", str(self.source), "add", "."], check=True)
        subprocess.run(["git", "-C", str(self.source), "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "fixture"], check=True)
        subprocess.run(["git", "-C", str(self.source), "remote", "add", "origin", "https://github.com/NousResearch/hermes-agent.git"], check=True)

    def select(self, python):
        self.receipt.write_text(json.dumps({"packages": {"venv": {"environment": str(python.parent.parent)}}}))

    def mutate(self, *_):
        self.source.joinpath("runtime.txt").write_text("new version")
        self.home.joinpath("config.yaml").write_text("migrated config")
        self.home.joinpath("migration-created.txt").write_text("new file")
        new = self.home / "installs/test/environments/new/venv/Scripts/python.exe"
        new.parent.mkdir(parents=True)
        new.write_bytes(b"new interpreter")
        self.select(new)
        return True

    def run_update(self, runner=None, probe=None):
        return update.transaction(self.root, self.old, "update", runner=runner or self.mutate,
                                  probe=probe or (lambda _: True), idle=lambda: None)

    def assert_restored(self):
        self.assertEqual(self.source.joinpath("runtime.txt").read_text(), "old version")
        self.assertEqual(self.home.joinpath("config.yaml").read_text(), "private synthetic config")
        self.assertFalse(self.home.joinpath("migration-created.txt").exists())
        self.assertEqual(update.selected_python(self.root), self.old)
        self.assertEqual(self.root.joinpath("codex-home/auth.json").read_text(), "synthetic account marker")
        self.assertFalse(self.root.joinpath(update.JOURNAL).exists())

    def test_success_keeps_new_selection_and_backup(self):
        calls = []
        def runner(python, source, args):
            calls.append(args)
            return self.mutate()
        self.assertEqual(self.run_update(runner), "updated")
        self.assertEqual(self.source.joinpath("runtime.txt").read_text(), "new version")
        self.assertIn("--no-gateway-restart", calls[0])
        self.assertIn("--keep-stash", calls[0])
        self.assertFalse(self.root.joinpath(update.JOURNAL).exists())
        self.assertEqual(len(list(self.root.joinpath("updates").glob("*/complete.json"))), 1)

    def test_incompatible_update_restores_state_and_dependency_selection(self):
        self.assertEqual(self.run_update(probe=lambda _: False), "rolled_back")
        self.assert_restored()

    def test_failed_native_update_restores(self):
        def fail(*args):
            self.mutate()
            return False
        self.assertEqual(self.run_update(fail), "rolled_back")
        self.assert_restored()

    def test_interruption_recovers_on_next_launch(self):
        def interrupt(*args):
            self.mutate()
            raise KeyboardInterrupt()
        with self.assertRaises(KeyboardInterrupt):
            self.run_update(interrupt)
        self.assertTrue(update.recover(self.root))
        self.assert_restored()
        self.assertFalse(update.recover(self.root))

    def test_backup_failure_never_invokes_native_update(self):
        with patch.object(update, "copy_tree", side_effect=OSError("disk full")):
            with self.assertRaises(OSError):
                self.run_update(lambda *_: self.fail("Updater ran before a complete backup"))
        self.assertFalse(self.root.joinpath(update.JOURNAL).exists())

    def test_interrupted_restoration_can_be_retried(self):
        def interrupt(*args):
            self.mutate()
            raise KeyboardInterrupt()
        with self.assertRaises(KeyboardInterrupt):
            self.run_update(interrupt)
        with patch.object(update, "copy_tree", side_effect=OSError("interrupted restore")):
            with self.assertRaises(OSError):
                update.recover(self.root)
        self.assertTrue(self.root.joinpath(update.JOURNAL).exists())
        self.assertTrue(update.recover(self.root))
        self.assert_restored()

    def test_incomplete_backup_is_rejected_before_clearing_source(self):
        backup = self.root / "updates" / ("a" * 32)
        backup.mkdir(parents=True)
        update.atomic_json(backup / "complete.json", {"schema": 1})
        update.atomic_json(self.root / update.JOURNAL, {"backup": "a" * 32})
        with self.assertRaises(ValueError):
            update.recover(self.root)
        self.assertEqual(self.source.joinpath("runtime.txt").read_text(), "old version")

    def test_wal_transactions_survive_rollback(self):
        db = sqlite3.connect(self.home / "state.db")
        self.addCleanup(db.close)
        db.execute("PRAGMA journal_mode=WAL")
        db.execute("CREATE TABLE messages (body TEXT)")
        db.execute("INSERT INTO messages VALUES ('synthetic complete message')")
        db.commit()
        # Back up the committed WAL, then close the original connection before restoration.
        def mutate(*args):
            db.close()
            self.mutate()
            with closing(sqlite3.connect(self.home / "state.db")) as current:
                current.execute("DELETE FROM messages")
                current.commit()
            return False
        self.assertEqual(self.run_update(mutate), "rolled_back")
        with closing(sqlite3.connect(self.home / "state.db")) as restored:
            self.assertEqual(restored.execute("SELECT body FROM messages").fetchone()[0], "synthetic complete message")

    def test_recovery_rejects_path_traversal(self):
        update.atomic_json(self.root / update.JOURNAL, {"backup": "../../outside"})
        with self.assertRaises(ValueError):
            update.recover(self.root)
        self.assertEqual(self.source.joinpath("runtime.txt").read_text(), "old version")

    def test_nested_plugin_dependencies_are_preserved(self):
        dependencies = self.home / "plugins/custom/node_modules"
        dependencies.mkdir(parents=True)
        marker = dependencies / "fixture.js"
        marker.write_text("original dependency")
        def mutate(*args):
            self.mutate()
            marker.write_text("changed dependency")
            return False
        self.assertEqual(self.run_update(mutate), "rolled_back")
        self.assertEqual(marker.read_text(), "original dependency")

    def test_receipt_selection_and_tool_metadata_are_restored(self):
        tools = self.home / "tools"
        tools.mkdir()
        facts = tools / "facts.json"
        facts.write_text('{"version":"old"}')
        def mutate(*args):
            self.mutate()
            facts.write_text('{"version":"new"}')
            return False
        self.assertEqual(self.run_update(mutate), "rolled_back")
        self.assertEqual(json.loads(facts.read_text())["version"], "old")

    def test_nonofficial_origin_rejected_before_backup(self):
        subprocess.run(["git", "-C", str(self.source), "remote", "set-url", "origin", "http://example.invalid/unsafe"], check=True)
        with self.assertRaises(ValueError):
            self.run_update()
        self.assertFalse(self.root.joinpath(update.JOURNAL).exists())

    def test_idle_gate_precedes_mutation(self):
        with self.assertRaises(ValueError):
            update.transaction(self.root, self.old, "update", idle=lambda: (_ for _ in ()).throw(ValueError("busy")))
        self.assertFalse(self.root.joinpath("updates").exists())

    def test_preflight_reads_native_leases_without_importing_hermes(self):
        psutil = types.SimpleNamespace(Process=lambda: types.SimpleNamespace(parents=lambda: []),
                                       process_iter=lambda _: [])
        with closing(sqlite3.connect(self.home / "state.db")) as db:
            db.execute("CREATE TABLE session_turn_leases (expires_at REAL)")
            db.execute("INSERT INTO session_turn_leases VALUES (999999999999)")
            db.commit()
        with patch.dict("sys.modules", {"psutil": psutil}), patch.dict(os.environ, {"HERMES_HOME": str(self.home)}):
            with self.assertRaisesRegex(ValueError, "Conversations"):
                update.assert_idle()
            with closing(sqlite3.connect(self.home / "state.db")) as db:
                db.execute("DELETE FROM session_turn_leases")
                db.commit()
            update.assert_idle()

    def test_preflight_rejects_pending_native_tasks(self):
        psutil = types.SimpleNamespace(Process=lambda: types.SimpleNamespace(parents=lambda: []),
                                       process_iter=lambda _: [])
        board = self.home / "kanban/boards/echo"
        board.mkdir(parents=True)
        with closing(sqlite3.connect(board / "kanban.db")) as db:
            db.execute("CREATE TABLE tasks (status TEXT)")
            db.execute("INSERT INTO tasks VALUES ('ready')")
            db.commit()
        with patch.dict("sys.modules", {"psutil": psutil}), patch.dict(os.environ, {"HERMES_HOME": str(self.home)}):
            with self.assertRaisesRegex(ValueError, "Background tasks"):
                update.assert_idle()

    def test_preflight_refuses_other_managed_python_processes(self):
        psutil = types.SimpleNamespace(Process=lambda: types.SimpleNamespace(parents=lambda: []),
            process_iter=lambda _: [types.SimpleNamespace(info={"pid": os.getpid() + 1, "exe": str(self.old)})])
        with patch.dict("sys.modules", {"psutil": psutil}), patch.dict(os.environ, {"HERMES_HOME": str(self.home)}):
            with self.assertRaisesRegex(ValueError, "Another process"):
                update.assert_idle()

    def test_repair_keeps_current_checkout(self):
        def repair(python, source, args):
            self.assertEqual(args[:2], ["-m", "hermes_cli.venv_sync"])
            return True
        self.assertEqual(update.transaction(self.root, self.old, "repair", runner=repair, probe=lambda _: True, idle=lambda: None), "repaired")
        self.assertEqual(self.source.joinpath("runtime.txt").read_text(), "old version")


if __name__ == "__main__":
    unittest.main()
