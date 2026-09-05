---
description: Block until a matching inbox record arrives (optionally one id, with a timeout)
argument-hint: "[--id <id>] [--timeout <secs>]"
allowed-tools: Bash(owl watch:*)
---

Arguments: "$ARGUMENTS"

Run `owl watch $ARGUMENTS` and show its output when it returns. Without `--timeout` it
blocks until a record arrives, so prefer `--timeout <secs>` and tell the user before running
how long it will block. A timeout exits non-zero without a record; say so in one line.

When a record arrived, offer `/owlpost:show <id>` for it.

Note: OWL-023 replaces this plain wrapper with a Monitor-based `on|off|status` command.
