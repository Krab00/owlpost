---
description: Live inbox watch for this session — arm it, stop it, or report its state and the stored default
argument-hint: "on|off|status"
allowed-tools: Bash(owl inbox:*), Monitor, TaskStop, Read, Write
---

Arguments: "$ARGUMENTS" — one of `on`, `off`, `status`; no argument means `status`.

The live watch is a background `Monitor` that polls `owl inbox --count --format plain`
every 5 seconds and prints the counter sentence only when it changes, so the session gets
one notification per new batch of questions or answers and nothing while the inbox is
quiet. The stored default lives in `$OWLPOST_HOME/plugin.json` as `{"watch": true|false}`;
resolve the home as `${OWLPOST_HOME:-$HOME/.config/owlpost}`. When the file is absent the
default is on. The `SessionStart` hook reads the same file and, only when the watch is not
`false`, injects a self-contained sentence starting `owlpost: before handling this prompt`
that carries the whole Monitor call below (`persistent: true`, description "owlpost inbox",
the poll script verbatim), so the first turn arms the watch without opening this file.

## `status` (or no argument)

1. Say whether a watch Monitor with the description "owlpost inbox" is running in this
   session (you started it, so you know; do not run any command to find out).
2. `Read` `plugin.json` in the home; report the stored default as `on` (file absent, no
   `watch` key, or `true`) or `off` (`false`).
3. One line, for example: `owlpost watch: running in this session; default on`.
4. When no "owlpost inbox" watch runs in this session and the stored default is on, arm it exactly as `on` does (same `Monitor` call, same script) and say `owlpost watch: running; default on` instead.

## `off`

1. `TaskStop` the running "owlpost inbox" Monitor, if there is one.
2. `Write` `plugin.json` in the home with exactly `{"watch": false}` so later sessions do
   not arm it.
3. Say: `owlpost watch: stopped; new sessions will not arm it (/owlpost:watch on to restore)`.

## `on`

1. `Write` `plugin.json` in the home with exactly `{"watch": true}`.
2. Arm the watch unless one is already running in this session: one `Monitor` call with
   `persistent: true` and the description "owlpost inbox", running this script:

```
prev=""; while true; do cur=$(owl inbox --count --format plain 2>/dev/null || true); [ "$cur" != "$prev" ] && [ -n "$cur" ] && echo "$cur"; prev="$cur"; sleep 5; done
```

   The script emits only on change: a line goes out when the counter sentence differs from
   the previous poll, never on every poll. `--format plain` prints nothing at zero, so the
   empty-string guard makes "back to zero" silent as well.
3. Say: `owlpost watch: running; default on`.

## Ground rules

- The watch only counts. It never runs `owl inbox` without `--count` (listing marks records
  seen), and it never runs owl show, owl draft or owl send. Nothing is opened, drafted or sent
  because of a watch event.
- When an event lands, say one line built from the counter, for example
  `🦉 owlpost: 1 new answer from Maciek`, and offer `/owlpost:inbox`; then wait for the human.
- Arm at most one watch per session. The `SessionStart` hook asks for it once through the
  injected sentence; `/owlpost:watch on` is the manual way to do the same, and `status`
  does it only when the default is on and nothing runs yet.
- Blocking on a single record (`owl watch [--id <id>] [--timeout <secs>]`, exit 4 on
  timeout) belongs in a terminal, not in this command.
