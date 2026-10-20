"""Apply Echo-owned routing through Hermes' native configuration APIs."""
import json
import os
import sys
from pathlib import Path


def main():
    from hermes_cli.config import load_config, save_config
    from hermes_cli.profiles import create_profile, get_profile_dir
    from hermes_cli import kanban_db as kb
    from agent.transports.hermes_tools_mcp_server import HERMES_TOOLS_MCP_SERVER_NAME

    home = Path(os.environ["HERMES_HOME"])
    supplied = json.loads((home.parent / "echo-config.json").read_text(encoding="utf-8"))
    worker_model = supplied.pop("echo_worker_model", "")
    supplied.pop("echo_managed_actors", None)
    config = load_config()
    # Rebuild only Echo's namespace: disabling a server must remove it rather
    # than leave an old credential reference active through a deep merge.
    config["mcp_servers"] = {name: entry for name, entry in config.get("mcp_servers", {}).items()
                             if not name.startswith("echo_mcp_")}
    config["toolsets"] = [name for name in config.get("toolsets", [])
                          if not name.startswith("echo_mcp_")]
    for key, value in supplied.items():
        if isinstance(value, dict) and isinstance(config.get(key), dict):
            config[key].update(value)
        else:
            config[key] = value
    # load_config exposes legacy gateway fields at the root, where they
    # outrank nested fields during canonicalization. Replace both spellings.
    if "multiplex_profiles" in supplied.get("gateway", {}):
        config["multiplex_profiles"] = supplied["gateway"]["multiplex_profiles"]
    config.setdefault("plugins", {}).setdefault("enabled", [])
    if "echo_integration" not in config["plugins"]["enabled"]:
        config["plugins"]["enabled"].append("echo_integration")
    config["toolsets"] = list(dict.fromkeys([*config.get("toolsets", []), "echo"]))
    config.setdefault("kanban", {})["dispatch_profiles"] = ["echo-codex"]
    # This is a complete, successfully loaded configuration. Deep merging the
    # raw file here would resurrect the removed Echo-owned server entries.
    save_config(config)
    kb.create_board("echo", name="Echo assistant tasks")
    try:
        worker_home = create_profile("echo-codex", no_alias=True)
    except FileExistsError:
        worker_home = get_profile_dir("echo-codex")
    # A worker has its own native profile. It cannot replace the fast model
    # used by the general/per-agent conversation sessions.
    worker_path = Path(worker_home) / "config.yaml"
    from ruamel.yaml import YAML
    worker = YAML(typ="safe").load(worker_path.read_text(encoding="utf-8")) if worker_path.is_file() else {}
    if not isinstance(worker, dict):
        raise ValueError("The private worker configuration must be a mapping")
    worker.update({
        # The app-server owns ChatGPT authentication in CODEX_HOME. Selecting
        # Hermes' direct openai-codex OAuth provider would require a second,
        # unrelated Hermes login before its runtime override is applied.
        "model": {"provider": "openai", "default": worker_model,
                  "openai_runtime": "codex_app_server", "codex_bin": os.environ["ECHO_EXECUTABLE"]},
        "providers": {**worker.get("providers", {}), "openai": {
            "base_url": "https://api.openai.com/v1", "api_key": "no-key-required"}},
        "fallback_providers": [], "toolsets": ["web", "kanban"],
        "kanban": {"dispatch_in_gateway": False},
        "terminal": {"security_mode": "approval-required"},
        "mcp_servers": {HERMES_TOOLS_MCP_SERVER_NAME: {"command": sys.executable,
                          "args": ["-m", "echo_bridge.worker_tools"],
                          "env": {"PYTHONPATH": str(home.parent / "bridge")}}},
    })
    # JSON is valid YAML; this profile is solely Echo-owned. Normal upstream
    # config and profiles are preserved by save_config above.
    worker_path.write_text(json.dumps(worker, indent=2), encoding="utf-8")
    from hermes_cli.codex_runtime_plugin_migration import migrate
    report = migrate(worker, codex_home=home.parent / "codex-home",
                     discover_plugins=False, default_permission_profile=":read-only",
                     expose_hermes_tools=False)
    if report.errors:
        raise RuntimeError("Could not configure the private native worker tools")


if __name__ == "__main__":
    main()
