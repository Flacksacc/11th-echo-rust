import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


class ConfigureGate(unittest.TestCase):
    def test_google_extension_uses_native_streaming_client_and_private_credential(self):
        with tempfile.TemporaryDirectory(prefix="echo-google-gate-") as temporary:
            root = Path(temporary)
            home = root / "home"
            provider = home / "plugins/model-providers/echo-google"
            provider.mkdir(parents=True)
            (provider / "__init__.py").write_text(
                "from echo_bridge.google_provider import register\nregister()\n", encoding="utf-8")
            (provider / "plugin.yaml").write_text(
                "name: echo-google-provider\nkind: model-provider\nversion: 0.1.0\n", encoding="utf-8")
            supplied = {"model": {"provider": "echo-google", "default": "gemini-3.5-flash-lite",
                                   "base_url": "https://generativelanguage.googleapis.com/v1beta"},
                        "providers": {"echo-fast": {"base_url": "https://generativelanguage.googleapis.com/v1beta",
                            "extra_body": {"thinking_config": {"thinkingLevel": "low"}},
                            "extra_headers": {"x-echo-fixture": "${env:ECHO_TEST_HEADER}"}}}}
            (root / "echo-config.json").write_text(json.dumps(supplied), encoding="utf-8")
            (home / "config.yaml").write_text(json.dumps(supplied), encoding="utf-8")
            script = """
from hermes_cli.runtime_provider import resolve_runtime_provider
from providers import get_provider_profile
from run_agent import AIAgent
import os
runtime = resolve_runtime_provider()
assert runtime['provider'] == 'echo-google'
assert runtime['api_key'] == os.environ['ECHO_FAST_MODEL_KEY']
profile = get_provider_profile('echo-google')
assert profile.build_extra_body()['thinking_config'] == {'thinkingLevel': 'low'}
agent = AIAgent(model='gemini-3.5-flash-lite', enabled_toolsets=[], quiet_mode=True,
                **{key: runtime.get(key) for key in ('provider','api_key','base_url','api_mode')})
from agent.gemini_native_adapter import GeminiNativeClient
assert isinstance(agent.client, GeminiNativeClient)
assert agent.client.api_key == os.environ['ECHO_FAST_MODEL_KEY']
assert agent.client._default_headers['x-echo-fixture'] == 'fixture-header'
agent.client.close()
print('Native Google routing verified')
"""
            result = subprocess.run([sys.executable, "-c", script],
                env={**os.environ, "HERMES_HOME": str(home),
                     "ECHO_FAST_MODEL_KEY": "synthetic-google-readiness-credential",
                     "ECHO_TEST_HEADER": "fixture-header"}, capture_output=True, text=True, timeout=45)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("Native Google routing verified", result.stdout)

    def test_native_configuration_stays_private_and_preserves_unrelated_settings(self):
        with tempfile.TemporaryDirectory(prefix="echo-config-gate-") as temporary:
            root = Path(temporary)
            home = root / "home"
            home.mkdir()
            (home / "config.yaml").write_text("custom_echo_test: keep\n", encoding="utf-8")
            (root / "echo-config.json").write_text(json.dumps({
                "model": {"provider": "custom:echo-fast", "default": "fixture"},
                "echo_worker_model": "", "toolsets": ["web"],
                "gateway": {"multiplex_profiles": False},
            }), encoding="utf-8")
            result = subprocess.run([sys.executable, "-m", "echo_bridge.configure"],
                env={**os.environ, "HERMES_HOME": str(home), "ECHO_EXECUTABLE": sys.executable},
                capture_output=True, text=True, timeout=60)
            self.assertEqual(result.returncode, 0, result.stderr)
            from ruamel.yaml import YAML
            config = YAML(typ="safe").load((home / "config.yaml").read_text(encoding="utf-8"))
            self.assertEqual(config["custom_echo_test"], "keep")
            from gateway.config import GatewayConfig
            self.assertFalse(GatewayConfig.from_dict(config).multiplex_profiles)
            worker = json.loads((home / "profiles/echo-codex/config.yaml").read_text(encoding="utf-8"))
            self.assertEqual(worker["model"]["openai_runtime"], "codex_app_server")
            resolved = subprocess.run([sys.executable, "-c",
                "from hermes_cli.runtime_provider import resolve_runtime_provider; "
                "r=resolve_runtime_provider(); assert r['api_mode']=='codex_app_server'; "
                "print('Native app-server routing verified')"],
                env={**os.environ, "HERMES_HOME": str(home / "profiles/echo-codex")},
                capture_output=True, text=True, timeout=30)
            self.assertEqual(resolved.returncode, 0, resolved.stderr)
            private = (root / "codex-home/config.toml").read_text(encoding="utf-8")
            import tomllib
            from agent.transports.hermes_tools_mcp_server import HERMES_TOOLS_MCP_SERVER_NAME
            projected = tomllib.loads(private)["mcp_servers"]
            self.assertIn("command", projected[HERMES_TOOLS_MCP_SERVER_NAME])
            self.assertNotIn("hermes_tools", projected)
            self.assertIn("echo_bridge.worker_tools", private)
            self.assertIn(":read-only", private)
            self.assertTrue((home / "kanban/boards/echo/board.json").is_file())

            config["mcp_servers"] = {"echo_mcp_0": {"url": "https://example.invalid/mcp"},
                                     "user_server": {"url": "https://example.invalid/user"}}
            YAML().dump(config, home / "config.yaml")
            worker["custom_worker_test"] = "keep"
            (home / "profiles/echo-codex/config.yaml").write_text(json.dumps(worker), encoding="utf-8")
            supplied = json.loads((root / "echo-config.json").read_text(encoding="utf-8"))
            supplied["mcp_servers"] = {}
            (root / "echo-config.json").write_text(json.dumps(supplied), encoding="utf-8")
            repeated = subprocess.run([sys.executable, "-m", "echo_bridge.configure"],
                env={**os.environ, "HERMES_HOME": str(home), "ECHO_EXECUTABLE": sys.executable},
                capture_output=True, text=True, timeout=60)
            self.assertEqual(repeated.returncode, 0, repeated.stderr)
            changed = YAML(typ="safe").load((home / "config.yaml").read_text(encoding="utf-8"))
            self.assertNotIn("echo_mcp_0", changed["mcp_servers"])
            self.assertIn("user_server", changed["mcp_servers"])
            changed_worker = json.loads((home / "profiles/echo-codex/config.yaml").read_text(encoding="utf-8"))
            self.assertEqual(changed_worker["custom_worker_test"], "keep")

    def test_inspection_mcp_does_not_expose_desktop_actuators(self):
        from echo_bridge.worker_tools import READ_ONLY_TOOLS
        self.assertIn("kanban_complete", READ_ONLY_TOOLS)
        self.assertNotIn("browser_click", READ_ONLY_TOOLS)
        self.assertNotIn("terminal", READ_ONLY_TOOLS)
