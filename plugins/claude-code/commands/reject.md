---
description: Discard an inbox record (the peer gets no answer)
argument-hint: "<id>"
allowed-tools: Bash(owl reject:*)
---

Arguments: "$ARGUMENTS"

Rejecting is final, so it needs one confirmation.

1. Tell the user which record `owl reject <id>` discards and that the peer gets no answer.
2. Ask for explicit confirmation with AskUserQuestion before running. Do not run anything
   until the user says yes.
3. Run `owl reject $ARGUMENTS` and show the output. On a non-zero exit, show the error line
   and stop.
