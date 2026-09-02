---
description: List the owlpost inbox and offer the next action (show, draft, edit, send, reject) for each record
allowed-tools: Bash(owl inbox:*), Bash(owl show:*)
---

Run `owl inbox` and present the rows to the user (id, peer, type, state, path, age).
Listing marks the records as seen; that is expected.

Then offer the actions from the owlpost skill for the record the user picks:
`owl show <id>`, `owl draft <id>`, `owl edit <id>`, `owl send <id>`, `owl reject <id>`.
Show every draft verbatim before offering to send it. Run `owl send` or `owl reject` only
after the user explicitly approves; never send on your own.

If the inbox is empty, say so in one line and stop.
