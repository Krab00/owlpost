---
description: Walk the owlpost inbox — show each question verbatim, handle consent (allow/deny), and pick draft/send/edit/reject with AskUserQuestion
allowed-tools: Bash(owl inbox:*), Bash(owl show:*), Bash(owl allow:*), Bash(owl deny:*), Bash(owl draft:*), Bash(owl edit:*), Bash(owl send:*), Bash(owl reject:*)
---

Walk the inbox record by record. The human never types an `owl` command: every choice is an
`AskUserQuestion` picker, and every command below runs on the pick that names it.

Two rules hold for the whole flow:

- The question text and every draft are printed verbatim in a code block before any picker.
  Never paraphrase, shorten or "improve" them; never offer a choice about a text the human
  has not seen in full.
- `owl send` runs only on an explicit "Send" pick. `owl allow --always` and `owl deny` run
  only on their pick. Never send, allow or deny on your own initiative.

## 1. List

Run `owl inbox --json`. Each row carries `id`, `from` (fingerprint), `from_name`, `state`,
`project`, `path` (`null` = the whole repository), `type` and `age`. Listing marks the rows
seen; that is expected. If the array is empty, say "owlpost inbox is empty" in one line and
stop.

Handle the records by state, in this order: `consent`, then `pending`, then `drafted`,
then `answer` records. After each record continue with the next one; after the last one
stop.

## 2. `consent` records (held until the human allows the peer)

For every record in state `consent`:

1. Run `owl show <id>` and print the question verbatim in a code block, headed by the peer
   name, the fingerprint, the project and the path (or "whole repository").
2. `AskUserQuestion` with four options:
   - **Allow once** — `owl allow <fingerprint> --once`: releases this peer's held questions
     without setting a policy; the next question is held again.
   - **Allow always** — `owl allow <fingerprint> --always`: sets policy `auto`, so future
     questions from this peer are answered without asking. The human confirms the peer's
     fingerprint out-of-band first (a call, a chat message, `owl whoami` on the peer's
     machine); a hand-added contact additionally needs `--i-verified-the-fingerprint`, which
     you pass only when the human says they did verify it. Say all of this in the option
     description.
   - **Deny** — `owl deny <fingerprint>`: policy `never`; held questions are denied and new
     ones get 403.
   - **Skip** — leave the record held and move on.
3. Run the picked command and show its output. On a non-zero exit show the error line and
   stop. After "Allow once" or "Allow always" the record is now `pending`: continue with it
   in step 3 right away, without listing again.

## 3. `pending` records (question with no draft yet)

- One `pending` record: run `owl show <id>` and print the question at once, verbatim in a
  code block with peer name, project and path.
- Several: print the table (id, peer, path, age), `AskUserQuestion` to pick one, then run
  `owl show <id>` for the pick and print the question verbatim.

Then `AskUserQuestion` with **Draft / Reject / Skip**:

- **Draft** — run `owl draft <id>` (the configured responder harness against this
  checkout), then continue with the record in step 4.
- **Reject** — run `owl reject <id>`; the peer gets no answer.
- **Skip** — leave it pending and move on.

## 4. `drafted` records (draft stored, not sent)

1. Run `owl show <id>` (unless the draft output is already in view from step 3) and print
   the draft verbatim in a code block, under the question it answers. When the draft's
   language differs from the question's language (a Polish question, an English draft),
   say so in one line before the picker and recommend Edit.
2. `AskUserQuestion` with **Send / Edit / Reject**:
   - **Send** — run `owl send <id>`: signs the draft and moves it to the outbox. This is the
     only pick that runs `owl send`.
   - **Edit** — `owl edit <id>` opens `$EDITOR`, which cannot run inside a session (see
     `commands/edit.md`): tell the human to run `owl edit <id>` in a terminal and say when
     they are done. Then run `owl show <id>` again, print the edited draft verbatim in a code
     block, and ask again with the same Send / Edit / Reject picker. Never send a draft the
     human has not seen after editing.
   - **Reject** — run `owl reject <id>`; the draft is discarded and the peer gets no answer.
3. Show the command's output. On a non-zero exit show the error line and stop; the record
   is untouched and the flow can be repeated.

Never chain `owl draft` and `owl send` in one step.

## 5. `answer` records (a peer answered a question asked from here)

Run `owl show <id>` and print the answer verbatim in a code block, with the peer name and
the question it replies to. Then apply the memory rule from the owlpost skill: only when the
human accepts the answer, save it as a `reference` fact with provenance
`owlpost:<peer>:<question id>` and today's date.
