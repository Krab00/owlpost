---
description: Update owl (binary + daemon) and reinstall this plugin
argument-hint: "[--source <repo dir>]"
allowed-tools: Bash(owl update:*)
---

Arguments: "$ARGUMENTS"

Run `owl update` with the given arguments (`--source <dir>` builds from a local checkout
instead of downloading the latest release; the new build replaces the `owl` that is actually
running, printed as `installed <path>`; the `owl` MCP server is registered at user scope
when missing). Show its output, then tell the user to restart
the Claude Code session so the reinstalled plugin loads.
