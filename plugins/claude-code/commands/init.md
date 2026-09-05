---
description: Create the owlpost home, key and config; print the fingerprint
argument-hint: "[--name <name>] [--email <email>]"
allowed-tools: Bash(owl init:*)
---

Arguments: "$ARGUMENTS"

Run `owl init $ARGUMENTS` (`--name` and `--email` fill the config; ask for them first when
they were not given) and show its output, including the fingerprint. If the home already
exists, show the error line and stop; nothing is overwritten.

Then offer the next steps: `/owlpost:install` for the daemon, `/owlpost:me` to hand the
peer file to a colleague, `/owlpost:add` for a colleague's peer file.
