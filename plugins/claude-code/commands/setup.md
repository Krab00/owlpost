---
description: First install in one go — owl init (if needed), the daemon service and this plugin (--dry-run lists the steps)
argument-hint: "[--name <name>] [--email <email>] [--plugin-source <dir|repo>] [--dry-run]"
allowed-tools: Bash(owl setup:*), Bash(owl doctor:*)
---

Arguments: "$ARGUMENTS"

If `--name` or `--email` is missing from the arguments, ask the user for it first, one
question at a time (name, then email), and pass both as flags (`owl setup` prompts for them
itself only on a terminal, never from here).

Run `owl setup $ARGUMENTS` and show its output. If the shell reports that `owl` is not found
(the plugin was installed before the binary), install the binary and run `owl setup` again;
it lands in `~/.local/bin`, so make sure that directory is on PATH:

```
curl -fsSL https://raw.githubusercontent.com/Krab00/owlpost/main/scripts/install.sh | sh
```

`owl setup` runs `owl init` unless a key exists, `owl install` for the daemon, then
`claude plugin marketplace add` and `claude plugin install` for this plugin (both no-ops when
the plugin is already installed), and registers the `owl` MCP server at user scope
(`claude mcp add`) unless it is already there. `--dry-run` lists the steps without running
them. On any other non-zero exit, show the error line and stop.

Afterwards run `owl doctor`, show the result, and tell the user to restart the Claude Code
session so the plugin loads.
