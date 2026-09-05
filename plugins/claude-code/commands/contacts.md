---
description: Pick a peer from the contact list with the arrow keys, then show their card or ask them a question
allowed-tools: Bash(owl contact:*), Bash(owl ask:*), Bash(git blame:*)
---

No arguments. Never ask the user to type a peer name; every choice below goes through the
`AskUserQuestion` tool so the list is navigated with the arrow keys and confirmed with Enter.

1. Run `owl contact list --json`. It prints a JSON array with one object per contact of the
   merged book (global `$OWLPOST_HOME/contacts/` plus this repository's `.agents/peers/`):
   `name`, `fingerprint`, `source` (`global` or `local`), `emails`, `endpoints` and, only
   when a policy is set, `policy` with `mode` (`manual`, `auto` or `never`).
   If the array is empty, print one line: "No contacts yet — run /owlpost:add with a
   colleague's peer file." and stop.
2. Present ONE `AskUserQuestion` ("Who do you want to ask?") with one option per contact:
   label = `name`; description = `fingerprint` + `source` + policy mode, where a missing
   `policy` shows as `-` (for example `owl:abcd1234efgh5678 · global · manual`). Do not add
   a reachability hint: `owl doctor` probes the whole setup, not one peer, so it is not
   cheap enough to run per contact.
3. After the pick, a second `AskUserQuestion`: "Ask <name> a question, or just show the
   card?" with the options "Ask a question" and "Show card".
   - "Show card": run `owl contact show <name>` and print the JSON in a code block. Stop.
   - "Ask a question": ask for the question text, then whether it is about one path (an
     optional file path; a question about the repository as a whole needs none). Then
     continue exactly as `/owlpost:ask <name> [path] <question>` would: follow steps 1–4 of
     `commands/ask.md` with the picked contact as `<peer>`. That flow already resolves the
     peer with `owl contact show`, shows peer, path and question, asks for the single explicit
     confirmation and only then runs `owl ask`. Do not add a second confirmation and do not
     run `owl ask` before it.
