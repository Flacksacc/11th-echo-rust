import json
import subprocess
import unittest
from types import SimpleNamespace
from unittest.mock import patch
from echo_bridge.plugin import _reminders


class ReminderBridgeTests(unittest.TestCase):
    def test_rule_patch_and_get_rule_use_bounded_hidden_broker(self):
        with patch("pathlib.Path.read_text", return_value=json.dumps({"echo_executable": "Echo.exe"})), patch("subprocess.run", return_value=SimpleNamespace(stdout='{"status":"saved"}')) as run:
            args = {"action": "rule", "scope": "global", "rule": {"repeat_minutes": 7}}
            self.assertEqual(json.loads(_reminders(args)), {"status": "saved"})
            self.assertEqual(run.call_args.args[0], ["Echo.exe", "--reminder-command"])
            self.assertEqual(json.loads(run.call_args.kwargs["input"]), args)
            self.assertEqual(run.call_args.kwargs["timeout"], 10)
            self.assertEqual(run.call_args.kwargs["creationflags"], getattr(subprocess, "CREATE_NO_WINDOW", 0))
            self.assertEqual(json.loads(_reminders({"action": "get_rule", "scope": "global"})), {"status": "saved"})

    def test_connection_operations_are_not_exposed_to_model(self):
        with patch("subprocess.run") as run:
            self.assertIn("error", json.loads(_reminders({"action": "connect"})))
            run.assert_not_called()

    def test_unavailable_broker_does_not_expose_diagnostics(self):
        with patch("pathlib.Path.read_text", side_effect=OSError("private diagnostic")):
            result = _reminders({"action": "list"})
            self.assertIn("unavailable", result)
            self.assertNotIn("private diagnostic", result)
