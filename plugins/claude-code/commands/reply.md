---
description: Reply to the question last shown in this session (else the newest pending one) — your own text, or a responder draft when no text is given
argument-hint: "[text]"
allowed-tools: Bash(owl inbox:*), Bash(owl show:*), Bash(owl draft:*), Bash(owl send:*), Bash(owl reject:*)
---

Arguments: "$ARGUMENTS"

No id to type: the record is the question last shown in this session (by `/owlpost:inbox`,
`/owlpost:show` or a hook wake). When none was shown, run `owl inbox --json` and take the
row with the highest `n` whose `type` is `question` and `state` is `pending` or `drafted`.
No such row: say "no pending question" and stop; if the only questions are `consent`, say
the peer is still held and offer `/owlpost:inbox`.

1. Run `owl show <id> --format claude` and paste its output verbatim (nothing before it,
   nothing inside it), so the human sees what they are replying to — skip this when that
   very table is the last thing in view.
2. Store the draft:
   - `$ARGUMENTS` non-empty — the human's own answer, byte for byte, through a quoted
     heredoc so no shell character is interpreted:

     ```
     owl draft <id> --text "$(cat <<'OWL_ANSWER'
     $ARGUMENTS
     OWL_ANSWER
     )"
     ```
   - `$ARGUMENTS` empty — `owl draft <id>`: the configured responder harness against this
     checkout.

   On a non-zero exit show the error line and stop.
3. Print the stored draft verbatim in a code block (never paraphrase or "improve" it), then
   `AskUserQuestion` with **Send / Save as draft / Edit / Reject** exactly as
   `commands/inbox.md` step 4: **Send** runs `owl send <id>` — the only pick that sends;
   **Save as draft** runs nothing; **Edit** — tell the human to run `owl edit <id>` in a
   terminal, then show the record again and ask again; **Reject** runs `owl reject <id>`.
   Never run `owl send` without that Send pick.
