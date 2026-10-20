"""Expose the native Hermes MCP server with Echo's inspection capability set.

Codex's OS permission profile gates its own commands. This separate tool gate
prevents an MCP browser actuator from bypassing that read-only default.
"""

READ_ONLY_TOOLS = (
    "web_search", "web_extract", "vision_analyze", "skill_view", "skills_list",
    "kanban_complete", "kanban_block", "kanban_request_review", "kanban_comment",
    "kanban_heartbeat", "kanban_show", "kanban_list",
)


def main():
    from agent.transports import hermes_tools_mcp_server as native
    native.EXPOSED_TOOLS = READ_ONLY_TOOLS
    return native.main()


if __name__ == "__main__":
    raise SystemExit(main())
