---
description: First install in one go — owl init (if needed), the daemon service and this plugin (--dry-run lists the steps)
argument-hint: "[--name <name>] [--email <email>] [--plugin-source <dir|repo>] [--dry-run]"
allowed-tools: Bash(owl setup:*), Bash(owl doctor:*)
---

Arguments: "$ARGUMENTS"

Run `owl setup $ARGUMENTS` and show its output. It runs `owl init` unless a key exists,
`owl install` for the daemon, then `claude plugin marketplace add` and `claude plugin install`
for this plugin, and registers the `owl` MCP server at user scope (`claude mcp add`) unless it
is already there. `--dry-run` lists the steps without running them. On a non-zero exit, show the
error line and stop.

Afterwards run `owl doctor`, show the result, and tell the user to restart the Claude Code
session so the plugin loads.
