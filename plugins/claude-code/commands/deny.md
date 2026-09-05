---
description: Deny a peer — policy never: held questions are denied, new ones get 403
argument-hint: "<peer>"
allowed-tools: Bash(owl deny:*)
---

Arguments: "$ARGUMENTS"

Denying changes trust, so it needs one confirmation.

1. Tell the user that `owl deny <peer>` sets policy `never`: held questions from the peer
   are denied and new ones are refused with 403.
2. Ask for explicit confirmation with AskUserQuestion before running. Do not run anything
   until the user says yes.
3. Run `owl deny $ARGUMENTS` and show the output. On a non-zero exit, show the error line
   and stop.
4. Mention that `/owlpost:allow <peer>` reverses it.
