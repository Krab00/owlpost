---
description: Ask a peer's agent a question about a path in their code (usage: /owlpost:ask <peer> <path> <question>)
argument-hint: "<peer> <path> <question>"
allowed-tools: Bash(owl ask:*), Bash(owl contact:*), Bash(git blame:*)
---

Arguments: "$ARGUMENTS"

Parse them as `<peer> <path> <question...>` (the question is everything after the path).
If any part is missing, ask the user for it instead of guessing.

1. Resolve the peer with `owl contact show <peer>` and show the user the peer's name and
   fingerprint, the path and the exact question text.
2. Ask for explicit approval. Do not run anything else until the user says yes.
3. Run `owl ask --file <path> --peer <peer> "<question>"` and report the result. On
   `accepted`, offer to wait for the answer with `owl ask ... --wait <secs>` only if the
   user wants to block; otherwise tell them the answer will show up in the inbox counter.
4. When an answer arrives and the user accepts it, follow the memory rule from the owlpost
   skill: save it with provenance `owlpost:<peer>:<question id>` and today's date.
