#!/bin/sh
# owlpost hook: print the Claude Code injection line for unseen questions, or nothing —
# and, on FileChanged, wake the session when a record arrives.
#
# Registered for SessionStart, UserPromptSubmit, PostToolUse and FileChanged (hooks.json).
# `owl inbox --count --format claude` prints exactly one JSON line when there are unseen
# questions and nothing at all when there are none, and never marks anything seen.
# Arguments are passed through (`--hook-event <name>`), and so is stdin: Claude Code's hook
# input JSON. On SessionStart the line also carries `watchPaths` (the spool inbox directory)
# unless $OWLPOST_HOME/plugin.json has `{"watch": false}`, so Claude Code runs the FileChanged
# hook when a record lands there. On FileChanged owl exits 2 with the counter sentence on
# stderr when a record was added and something is unseen — the exit code Claude Code's
# asyncRewake hook turns into a new model turn — and the script lets exactly that through;
# every other outcome is a silent exit 0. The script is a strict no-op (empty stdout, exit 0)
# when `owl` is not on PATH or fails for any reason, e.g. an uninitialised home, so a broken
# install can never block a Claude session.
command -v owl >/dev/null 2>&1 || exit 0
case " $* " in
*" FileChanged "*)
    err=$(owl inbox --count --format claude "$@" 2>&1 >/dev/null)
    [ $? -eq 2 ] || exit 0
    printf '%s\n' "$err" >&2
    exit 2
    ;;
esac
owl inbox --count --format claude "$@" 2>/dev/null || exit 0
