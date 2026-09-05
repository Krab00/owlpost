---
description: Install the owl daemon as a launchd/systemd service (--dry-run prints the unit)
argument-hint: "[--dry-run]"
allowed-tools: Bash(owl install:*)
---

Arguments: "$ARGUMENTS"

Run `owl install $ARGUMENTS` and show its output. `--dry-run` prints the unit that would be
written without installing anything. On a non-zero exit, show the error line and stop.

Afterwards offer `/owlpost:doctor` to check that the daemon is running.
