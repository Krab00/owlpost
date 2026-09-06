---
description: Ask a peer's agent a question about their code, optionally about one path (usage: /owlpost:ask <peer> [path] <question>)
argument-hint: "<peer> [path] <question...>"
allowed-tools: Bash(owl ask:*), Bash(owl contact:*), Bash(git blame:*)
---

Arguments: "$ARGUMENTS"

Parse them as `<peer> [path] <question...>`: the first word is the peer; if the second word
looks like a file path (contains `/` or a file extension, and is not a question word) it is the
path and the question is everything after it, otherwise the question is everything after the
peer and there is no path. A question about the repository as a whole ("how long is your
README?", "which branch do you deploy from?") needs no path — do not ask for one. Ask the user
only when the peer or the question itself is missing.

1. Resolve the peer with `owl contact show <peer>`.
2. Do not ask for confirmation: the command is the approval. Run at once. With a path, run
   `owl ask --file <path> --peer <peer> "<question>"`; without one, run
   `owl ask <peer> "<question>"`.
3. Report in one line who the question went to (name and fingerprint), the path (or
   "whole repository") and the result. On `accepted`, offer to wait for the answer with
   `owl ask ... --wait <secs>` only if the user wants to block; otherwise tell them the
   answer will show up in the inbox counter.
4. When an answer arrives and the user accepts it, follow the memory rule from the owlpost
   skill: save it with provenance `owlpost:<peer>:<question id>` and today's date.
