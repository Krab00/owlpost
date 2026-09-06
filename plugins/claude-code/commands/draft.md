---
description: Run the responder harness against this checkout and store a draft answer
argument-hint: "<id> [--harness <name>] [--send]"
allowed-tools: Bash(owl draft:*), Bash(owl send:*)
---

Arguments: "$ARGUMENTS"

Run `owl draft $ARGUMENTS` (`--harness <name>` picks another configured harness) and show
its output. Then show the stored draft verbatim, in a code block; do not paraphrase,
shorten or "improve" it. On a non-zero exit, show the error line and stop.

Offer `/owlpost:send <id>` to sign and send it, `/owlpost:edit <id>` to change it, or
`/owlpost:reject <id>` to discard the record.

`/owlpost:draft <id> --send` does draft, print, send in one go: `--send` is stripped from the
arguments before `owl draft` runs (owl has no such flag), the draft is printed verbatim as
above, then `owl send <id>` runs at once. On a non-zero `owl draft` exit show the error line
and stop (nothing is sent); on a non-zero `owl send` exit show the error line, the record
stays `drafted`.

Never chain `owl draft` and `owl send` unless the human picked "Draft & send" (or passed `--send`); the draft is still printed in full before `owl send` runs.
