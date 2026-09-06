---
name: owlpost
description: Ask a colleague's local agent about code they own and answer their questions from this checkout with owlpost (`owl ask`, `owl inbox`, `owl draft`, `owl send`). Use when the user names a colleague, asks who to ask about a file, mentions owlpost, or when a hook injected an "owlpost: N new questions" or "N new answers" line.
---

# owlpost

owlpost lets this machine ask a peer's local agent about code the peer owns, and lets the
peer ask about code owned here. Every message crosses the wire only after a human has read
it. The `owl` CLI is the only interface; never edit files under `$OWLPOST_HOME` directly.

## Ground rule

Sending anything to a peer requires explicit human approval. Never run `owl send` or
`owl ask` on your own initiative; describe what would go out, wait for a clear yes, then
run the command. An `/owlpost:ask` the user typed, a mentioned `@owl:to://` contact, or the
user asking in their own words ("ask Maciek why this retries") is that approval: run it
without a confirmation step.

## Slash commands

Every `owl` subcommand except `daemon` and `mcp` has a `/owlpost:<name>` command that runs
it with the arguments and offers the next step; prefer them over typing `owl` when the user
is in a session. `/owlpost:me` is `owl contact export`, `/owlpost:contacts` is the table
over `owl contact list` ending with the `@owl:to://` mention hint.

- Setup: `/owlpost:setup` (init + daemon + plugin in one go), `/owlpost:init`, `/owlpost:whoami`, `/owlpost:me`, `/owlpost:card`,
  `/owlpost:install`, `/owlpost:uninstall`, `/owlpost:doctor`, `/owlpost:update`
- Contacts and trust: `/owlpost:contacts`, `/owlpost:contact`, `/owlpost:add`,
  `/owlpost:allow`, `/owlpost:deny`
- Asking: `/owlpost:ask`, `/owlpost:history`, `/owlpost:watch`
- Answering: `/owlpost:inbox`, `/owlpost:show`, `/owlpost:draft`, `/owlpost:edit`,
  `/owlpost:send`, `/owlpost:reject`

`/owlpost:send`, `/owlpost:allow`, `/owlpost:deny`, `/owlpost:reject` and
`/owlpost:uninstall` ask for one explicit confirmation before running.
`/owlpost:ask` does not: the command itself is the approval.

## When to ask a peer

Suggest `owl ask` when:

- the user names a colleague ("ask Maciek why this retries", "Ana wrote this, check with her");
- the user asks about code whose `git blame` points at a contact, and the answer is not
  in the repo, its docs, or the commit history;
- the user asks "who should I ask about `<path>`".

When the user wants to ask a peer but does not name one (or is unsure of the spelling),
point them at `/owlpost:contacts`: it prints the book as a table and the hint to mention a
contact through the `owl` MCP resources (see "Mentioned contact" below).

Do not ask a peer for anything answerable from the checkout. Prefer reading the code first.

## Mentioned contact

`owl mcp` (registered in Claude Code at user scope by `owl setup` / `owl update`) serves
every contact as a resource `to://<name-slug>.<email>` of the `owl` server; Claude Code
lists them in the `@` typeahead and attaches the picked one to the prompt. To mention one,
type @owl: and the start of the name (or e-mail), pick with the arrows, then type the
question. An attached `@owl:to://…` resource in the user's prompt is the peer: its content
is `{"name","fingerprint","emails"}`.

- The rest of the prompt is `[path] <question>` (a path when a word contains `/` or a file
  extension and is not a question word; otherwise the whole text is the question).
- Run `owl ask --peer <fingerprint> "<question>"` at once, with `--file <path>` when a path
  is given: the mention is the approval, do not confirm first.
- Report who it went to (name and fingerprint), the path or "whole repository", and the
  result, as `/owlpost:ask` does.
- Several mentions send the same question to each contact, one `owl ask` per mention.

## How to run `owl ask`

```
owl ask --file <path> "<question>"          # proposes peers from git blame of <path>
owl ask --file <path> --peer <peer> "<question>"
owl ask <peer> <path> "<question>"          # peer: name prefix, email or fingerprint
owl ask <peer> "<question>"                  # no path: a question about the repository as a whole
```

1. Prefer `--file <path>`. It proposes peers from `git blame` of that file and uses the
   file as the question's path. Add `--peer` only when the user already named the peer.
2. When the user asked for it (an `/owlpost:ask`, an `@owl:to://` mention, or "ask X …" in
   their own words), run the command at once and report the peer (name and fingerprint),
   the path and the question with the result. Only when *you* proposed asking a peer, show
   those first and wait for a clear yes.
3. Read the result. `accepted` means the question was delivered; add `--wait <secs>` when
   the user wants to block for the answer. Exit code 4 means the wait timed out; the answer
   will still arrive in the inbox later. Do not retry a rejected or denied question.
4. Use `--project <id>` only when the repo has no `origin` remote.

## Reacting to the injected counter

A hook injects a line such as
`owlpost: 2 new questions (Maciek 2). Say "show owlpost inbox" or run `owl inbox`.`
at session start, on every prompt and after tool calls.

- Mention it once, briefly, and offer to show the inbox. Never dump question content
  unasked, and never draft or send anything because of the counter alone.
- If the user is mid-task and does not react, do not bring it up again until the count
  changes.
- When the user says "show owlpost inbox" (or similar), run `owl inbox` and summarise the
  rows: id, peer, path, age.

## First turn

When the `SessionStart` context lists unseen records (one `- <peer> <kind> [<state>] on
<path>: <first line>` line each, closed by `owlpost: run /owlpost:inbox now.`), run
`/owlpost:inbox` on the first turn before anything else: consent first (allow/deny via
AskUserQuestion), then draft/send/reject per record. Do not ask whether to open it; the
sentence is the instruction. Then handle the user's prompt.

## Live watch

The `SessionStart` hook may inject `owlpost: arm the inbox watch (see /owlpost:watch)`
next to the counter. When it does, arm the watch once per session, on the first turn,
silently: one `Monitor` call with `persistent: true`, description "owlpost inbox", running
the poll script from `commands/watch.md`. Do not mention the arming to the user, and
never arm a second watch in the same session. No sentence means the user switched it off
with `/owlpost:watch off`; do not arm it then.

- On a watch event, report it in one line, built from the counter, for example
  `owlpost: 1 new answer from Maciek`, and offer `/owlpost:inbox`.
- The watch is a counter only: never list the inbox, show, draft or send anything because
  of a watch event. Nothing is opened, drafted or sent without the human's pick.
- `/owlpost:watch off` stops it for this session and later ones, `/owlpost:watch on`
  restores it, `/owlpost:watch status` reports both.

## The answer loop

Every step needs the human's go-ahead before moving to the next one.

1. `owl inbox` lists inbox records and marks them seen (`owl inbox --new` lists only
   unseen ones; `owl inbox --count` never marks anything). `/owlpost:inbox` walks the
   records below with `AskUserQuestion` pickers, so the human never types an `owl` command.
2. A record in state `consent` is a question from a peer who has no policy yet; it is held
   until the human decides. `owl show <id>` prints the question; show it verbatim, in a
   code block, with the peer's name and fingerprint. Then, only on the human's pick:
   - `owl allow <peer> --once` releases the held questions to `pending` without a policy;
   - `owl allow <peer> --always` sets policy `auto` (future questions answered without
     asking) — only after the human confirmed the peer's fingerprint out-of-band; a
     hand-added contact additionally needs `--i-verified-the-fingerprint`;
   - `owl deny <peer>` sets policy `never`: held questions are denied, new ones get 403.
   Never allow or deny on your own initiative.
3. A `pending` record is a question with no draft yet. `owl show <id>` prints the full
   question: who asked, project, path, text. Show it verbatim before offering anything.
4. `owl draft <id>` runs the configured responder harness against this checkout and stores
   a draft answer (`--harness <name>` picks another configured harness).
5. Show the draft verbatim to the human, in a code block. Do not paraphrase, shorten or
   "improve" it silently. Say so when the draft's language differs from the question's.
6. Only on explicit human approval:
   - `owl send <id>` signs the draft and moves it to the outbox; a "Draft & send" pick in
     `/owlpost:inbox` (or `/owlpost:draft <id> --send`) is that approval given once, at
     draft time: the draft is still printed in full before `owl send` runs;
   - `owl edit <id>` opens the draft in `$EDITOR` when the human wants changes, then show
     the edited draft again and ask again before `owl send`;
   - `owl reject <id>` discards the record when the human declines to answer.

Never chain `owl draft` and `owl send` unless the human picked "Draft & send" (or passed `--send`); the draft is still printed in full before `owl send` runs.
Never send a draft the human has not seen in full.

## Showing messages

Received messages (answers from peers, history rows) are shown as one table, newest last:
the left column is the local time (`HH:MM`) and the peer's name, the right column the
message text verbatim (no paraphrase; escape `|` inside a cell). Show only messages from
the last 24 hours, at most the 10 newest; say in one line how many older ones were left
out, and show them only when the human asks for them. Questions and drafts that wait for a
decision are still printed in full, in a code block (see the answer loop). Markdown tables
have no row background, so each peer gets one fixed colour marker at the start of the left
cell instead, in order of first appearance in the session: 🟦 🟩 🟨 🟪 🟧 🟥 (then repeat);
the same peer keeps the same marker for the whole session.

| 🟦 00:16 · Krzysztof Abramczyk | Wszystko ok, ale późno już. |
|---|---|
| 🟦 00:21 · Krzysztof Abramczyk | Pewnie koło 22. |
| 🟩 00:24 · Ana Kowalska | Retry lives in `auth/session.rs`. |

## Memory rule

When the human accepts an answer that came back from a peer (via `owl ask --wait`, or a
later `owl show <id>` of an answer record), save it to memory as a `reference` fact with
provenance `owlpost:<peer>:<question id>` and today's date, for example:

```
Source: owlpost:maciek:q_01J8Z...  (2026-09-02)
```

Save only answers the human has read and accepted. Never store a peer's answer as your own
knowledge without the provenance line, and never store questions or drafts from the inbox.

## Contacts

`owl contact list` shows every contact with its scope: `global` = this machine's own book
(`$OWLPOST_HOME/contacts/`, every repo), `local` = this repository's `.agents/peers/`
(committed, shared via PR). `--global` / `--local` list one scope. `owl add <file|json|->`
adds a peer file to the global book (`--local` for the repo); `owl contact remove <peer>`
(`--local` for the repo) deletes one. Adding never grants access: `owl allow <peer>` does,
after the fingerprint was confirmed out-of-band.

## History

`owl history` lists finished exchanges; filter with `--peer <peer>`, `--path <path>` and
`--since <when>`. Use it before asking a peer the same thing twice.
