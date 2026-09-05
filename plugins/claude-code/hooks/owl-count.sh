#!/bin/sh
# owlpost hook: print the Claude Code injection line for unseen questions, or nothing.
#
# Registered for SessionStart, UserPromptSubmit and PostToolUse (hooks.json). `owl inbox
# --count --format claude` prints exactly one JSON line when there are unseen questions and
# nothing at all when there are none, and never marks anything seen. Arguments are passed
# through: hooks.json adds `--session-start` on SessionStart only, which makes the line carry
# the "arm the inbox watch" sentence (OWL-023) unless $OWLPOST_HOME/plugin.json has
# `{"watch": false}`. The script is a strict no-op (empty stdout, exit 0) when `owl` is not
# on PATH or fails for any reason, e.g. an uninitialised home, so a broken install can never
# block a Claude session.
command -v owl >/dev/null 2>&1 || exit 0
owl inbox --count --format claude "$@" 2>/dev/null || exit 0
