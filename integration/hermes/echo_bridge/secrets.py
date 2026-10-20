"""Expose Echo's process-injected credentials through Hermes' native scope.

Hermes multiplexing deliberately ignores process-environment credentials on
scope misses. A native secret source binds only Echo's private default home;
worker and sibling profiles receive none of its credentials.
"""
import os
from pathlib import Path
import re
from agent.secret_sources.base import SecretSource, FetchResult, ErrorKind

_NAMES = re.compile(r"ECHO_MCP_[0-9]+_(?:ENV|HEADER)_[0-9]+\Z")


class EchoSecretSource(SecretSource):
    name = "echo"
    label = "Echo protected settings"

    def __init__(self, owner_home=None):
        self.owner_home = (owner_home or Path(__file__).resolve().parents[2] / "home").resolve()

    def fetch(self, cfg, home_path):
        try:
            allowed = Path(home_path).resolve() == self.owner_home
        except (OSError, ValueError):
            allowed = False
        if not allowed:
            return FetchResult().fail("Echo credentials belong to its private conversation profile",
                                      ErrorKind.NOT_CONFIGURED)
        names = {"ECHO_FAST_MODEL_KEY", "ECHO_FAST_MODEL_HEADER", "API_SERVER_KEY"}
        return FetchResult(secrets={name: value for name, value in os.environ.items()
            if value and (name in names or _NAMES.fullmatch(name))})
