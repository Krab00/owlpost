#!/bin/sh
# owlpost hook: print the Claude Code injection line for unseen questions, or nothing.
#
# Registered for SessionStart, UserPromptSubmit and PostToolUse (hooks.json). `owl inbox
# --count --format claude` prints exactly one JSON line when there are unseen questions and
# nothing at all when there are none, and never marks anything seen. Arguments are passed
# through (`--hook-event <name>`), and so is stdin: Claude Code's hook input JSON, from which
# owl takes `session_id`. On SessionStart and UserPromptSubmit the line carries the "arm the
# inbox watch" sentence (OWL-029) unless $OWLPOST_HOME/plugin.json has `{"watch": false}` or
# a live watch marker $OWLPOST_HOME/watch/<session id> exists. The script is a strict no-op
# (empty stdout, exit 0) when `owl` is not on PATH or fails for any reason, e.g. an
# uninitialised home, so a broken install can never block a Claude session.
command -v owl >/dev/null 2>&1 || exit 0
owl inbox --count --format claude "$@" 2>/dev/null || exit 0
