import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from echo_bridge.secrets import EchoSecretSource


class SecretScopeGate(unittest.TestCase):
    def test_native_multiplex_scope_gets_credentials_without_sibling_leakage(self):
        from hermes_cli.env_loader import hydrate_profile_secret_sources
        from agent.secret_sources.registry import register_source
        from agent.secret_scope import build_profile_secret_scope
        from hermes_constants import hermes_home_key
        with tempfile.TemporaryDirectory(prefix="echo-secret-gate-") as temporary:
            home = Path(temporary) / "home"
            home.mkdir()
            (home / "config.yaml").write_text("secrets:\n  echo:\n    enabled: true\n", encoding="utf-8")
            source = EchoSecretSource(home)
            register_source(source, scope=hermes_home_key(home))
            with patch.dict(os.environ, {"ECHO_FAST_MODEL_KEY": "synthetic-conversation-key",
                    "ECHO_MCP_0_ENV_0": "synthetic-mcp-key", "API_SERVER_KEY": "synthetic-gateway-key",
                    "UNRELATED_SECRET": "synthetic-unrelated"}):
                hydrate_profile_secret_sources(home)
                scope = build_profile_secret_scope(home)
                self.assertEqual(scope["ECHO_FAST_MODEL_KEY"], "synthetic-conversation-key")
                self.assertEqual(scope["ECHO_MCP_0_ENV_0"], "synthetic-mcp-key")
                self.assertNotIn("UNRELATED_SECRET", scope)
                worker = home / "profiles/echo-codex"
                worker.mkdir(parents=True)
                self.assertFalse(source.fetch({}, worker).ok)
                self.assertNotIn("ECHO_FAST_MODEL_KEY", build_profile_secret_scope(worker))
