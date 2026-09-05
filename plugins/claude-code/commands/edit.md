---
description: Edit a draft answer (owl edit opens $EDITOR, which cannot run inside Claude Code)
argument-hint: "<id>"
allowed-tools: Bash(owl edit:*)
---

Arguments: "$ARGUMENTS"

`owl edit <id>` opens the draft in `$EDITOR`, an interactive terminal program that cannot
run inside a Claude Code session. Do not run it here. Instead:

1. Tell the user that and offer the two alternatives: run `owl edit $ARGUMENTS` in a
   terminal themselves, or paste the edited text here.
2. If they paste the text, show it back verbatim in a code block and say that the draft on
   disk is only changed by `owl edit` in a terminal; the pasted text is not saved.
3. After the draft was edited, offer `/owlpost:show <id>` to review it and only then
   `/owlpost:send <id>`.
