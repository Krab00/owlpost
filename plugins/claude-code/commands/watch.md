---
description: Live inbox watch for this session — arm it, stop it, or report its state and the stored default
argument-hint: "on|off|status"
allowed-tools: Bash(owl inbox:*), Monitor, TaskStop, Read, Write
---

Arguments: "$ARGUMENTS" — one of `on`, `off`, `status`; no argument means `status`.

The live watch is a background `Monitor` running `owl inbox --count --follow` for this
session: it polls the count every 5 seconds and prints the counter sentence only when it
changes, so the session gets one notification per new batch of questions or answers and
nothing while the inbox is quiet (`--follow` prints nothing at zero, so "back to zero" is
silent as well). The stored default lives in `$OWLPOST_HOME/plugin.json` as
`{"watch": true|false}`; resolve the home as `${OWLPOST_HOME:-$HOME/.config/owlpost}`. When
the file is absent the default is on.

The `SessionStart` and `UserPromptSubmit` hooks read the same file and, only when the watch
is not `false` and no watch is live for this session, inject a self-contained sentence
starting `owlpost: before handling this prompt` that carries the whole Monitor call below
(`persistent: true`, description "owlpost inbox", the command verbatim with this session's
id filled in). A running watch leaves a marker `$OWLPOST_HOME/watch/<session id>` holding
its pid; the hook sees the marker and stops asking. So the sentence may arrive on any turn,
and its presence always means no watch is live for this session.

The command template (the sentence carries it with `<session id>` replaced):

```
owl inbox --count --follow --session <session id>
```

## `status` (or no argument)

1. Say whether a watch Monitor with the description "owlpost inbox" is running in this
   session (you started it, so you know; do not run any command to find out).
2. `Read` `plugin.json` in the home; report the stored default as `on` (file absent, no
   `watch` key, or `true`) or `off` (`false`).
3. One line, for example: `owlpost watch: running in this session; default on`.
4. When no "owlpost inbox" watch runs in this session and the stored default is on, arm it exactly as `on` does (same `Monitor` call, the command from this turn's sentence) and say `owlpost watch: running; default on` instead.
5. When this turn carries no sentence and the default is on, the watch is already live for this session (the hook saw its marker): say `owlpost watch: running; default on`.

## `off`

1. `TaskStop` the running "owlpost inbox" Monitor, if there is one (its marker disappears
   with the process).
2. `Write` `plugin.json` in the home with exactly `{"watch": false}` so later sessions do
   not arm it.
3. Say: `owlpost watch: stopped; new sessions will not arm it (/owlpost:watch on to restore)`.

## `on`

1. `Write` `plugin.json` in the home with exactly `{"watch": true}`.
2. When this turn's context carries the `owlpost: before handling this prompt` sentence, arm
   the watch from it unless one is already running in this session: one `Monitor` call with
   `persistent: true`, the description "owlpost inbox" and the command from the sentence
   (the template above with this session's id). Say: `owlpost watch: running; default on`.
3. When this turn carries no sentence, do not guess a session id: say `owlpost watch: on; it arms on your next prompt` (the next `UserPromptSubmit` hook injects the sentence, and you arm it then).

## Ground rules

- The watch only counts. It never runs `owl inbox` without `--count` (listing marks records
  seen), and it never runs owl show, owl draft or owl send. Nothing is opened, drafted or sent
  because of a watch event.
- When an event lands, say one line built from the counter, for example
  `🦉 owlpost: 1 new answer from Maciek`, and offer `/owlpost:inbox`; then wait for the human.
- Arm at most one watch per session. The hooks ask for it through the injected sentence
  whenever none is live; `/owlpost:watch on` is the manual way to do the same, and `status`
  does it only when the default is on and nothing runs yet.
- Blocking on a single record (`owl watch [--id <id>] [--timeout <secs>]`, exit 4 on
  timeout) belongs in a terminal, not in this command.
