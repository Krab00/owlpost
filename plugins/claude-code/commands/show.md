---
description: Show the full content of an inbox or history record (marks it seen)
argument-hint: "<id>"
allowed-tools: Bash(owl show:*)
---

Arguments: "$ARGUMENTS"

Run `owl show $ARGUMENTS` and show the output verbatim: who asked, project, path, the
question text, and the draft or answer when there is one. Showing marks the record seen;
that is expected. On a non-zero exit, show the error line (unknown id) and stop.

Then offer the next step for the record: `/owlpost:draft <id>` when there is no draft yet,
`/owlpost:send <id>` or `/owlpost:reject <id>` when there is one. Never run any of them on
your own.
