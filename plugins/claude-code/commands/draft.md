---
description: Run the responder harness against this checkout and store a draft answer
argument-hint: "<id> [--harness <name>]"
allowed-tools: Bash(owl draft:*)
---

Arguments: "$ARGUMENTS"

Run `owl draft $ARGUMENTS` (`--harness <name>` picks another configured harness) and show
its output. Then show the stored draft verbatim, in a code block; do not paraphrase,
shorten or "improve" it. On a non-zero exit, show the error line and stop.

Offer `/owlpost:send <id>` to sign and send it, `/owlpost:edit <id>` to change it, or
`/owlpost:reject <id>` to discard the record. Never chain draft and send in one step.
