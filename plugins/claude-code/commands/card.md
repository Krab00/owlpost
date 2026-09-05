---
description: Print this machine's card, or fetch and print a peer's card
argument-hint: "[peer]"
allowed-tools: Bash(owl card:*)
---

Arguments: "$ARGUMENTS"

Run `owl card $ARGUMENTS`: with no argument it prints this machine's own card, with a peer
(name prefix, email or fingerprint) it fetches and prints that peer's card. Show the output
in a code block. If the peer cannot be reached, show the error line and offer
`/owlpost:doctor`.
