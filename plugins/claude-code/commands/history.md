---
description: Show finished owlpost exchanges, optionally filtered by peer, path or age
argument-hint: "[--peer <peer>] [--path <path>] [--since <when>]"
allowed-tools: Bash(owl history:*), Bash(owl show:*)
---

Arguments: "$ARGUMENTS"

Run `owl history` with the given filters (`--peer`, `--path`, `--since`) and present the
rows as the message table from the owlpost skill ("Showing messages": time and peer on the
left, text on the right, last 24 hours, at most the 10 newest, say how many were left out).
Offer `owl show <id>` for the full content of any exchange the user picks.

Use this before asking a peer a question that may already have been answered.
