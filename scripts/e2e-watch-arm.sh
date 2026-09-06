#!/bin/sh
# e2e-watch-arm.sh — proof on the real Claude Code harness that the owlpost plugin arms the
# inbox watch (OWL-029, design §12). For each of three first prompts it starts `claude -p` in
# a throwaway OWLPOST_HOME and checks that a watch really came up: the marker
# $OWLPOST_HOME/watch/<session id> holds a live pid and the stream carries a `Monitor`
# tool_use with description "owlpost inbox" and `--session <that id>`.
#
#   scripts/e2e-watch-arm.sh                       # installed plugin, the `owl` on PATH
#   E2E_PLUGIN_DIR=plugins/claude-code PATH=$PWD/target/release:$PATH scripts/e2e-watch-arm.sh
#
#   E2E_PLUGIN_DIR   plugin directory to load with `--plugin-dir` (default: none — the plugin
#                    installed at user scope is what runs)
#   E2E_OUT          directory that keeps the three stream files as evidence (default: a temp
#                    dir, printed at the end)
#
# The persistent Monitor keeps `claude -p` alive, so every run is bounded by `timeout 180`
# and killed once the check is done. Prints `PASS <prompt>` / `FAIL <prompt> (<reason>)` per
# prompt and exits non-zero on any FAIL (2 = precondition). Cost: three `claude -p` calls.
set -u

fail_pre() {
    echo "e2e-watch-arm: $1" >&2
    exit 2
}

command -v claude >/dev/null 2>&1 || fail_pre "claude not found on PATH"
command -v owl >/dev/null 2>&1 || fail_pre "owl not found on PATH"
command -v pgrep >/dev/null 2>&1 || fail_pre "pgrep not found on PATH"

if [ -n "${E2E_PLUGIN_DIR:-}" ]; then
    [ -d "$E2E_PLUGIN_DIR" ] || fail_pre "E2E_PLUGIN_DIR=$E2E_PLUGIN_DIR is not a directory"
    E2E_PLUGIN_DIR=$(cd "$E2E_PLUGIN_DIR" && pwd)
fi
OUT=${E2E_OUT:-$(mktemp -d)}
mkdir -p "$OUT" || fail_pre "cannot create E2E_OUT=$OUT"
WORK=$(mktemp -d)

echo "owl: $(command -v owl) ($(owl --version 2>/dev/null))"
echo "claude: $(command -v claude) ($(claude --version 2>/dev/null | head -n 1))"
echo "plugin: ${E2E_PLUGIN_DIR:-installed at user scope}"
echo "out: $OUT"

# kill_tree <pid>: the process and everything below it (claude, its hooks, the follow).
kill_tree() {
    for child in $(pgrep -P "$1" 2>/dev/null); do
        kill_tree "$child"
    done
    kill "$1" 2>/dev/null || true
}

# pid_alive <pid>
pid_alive() {
    kill -0 "$1" 2>/dev/null
}

# session_id <stream file>: the first "session_id" value in the stream.
session_id() {
    sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p' "$1" | head -n 1
}

failures=0
n=0
for prompt in "/owlpost:watch status" "/owlpost:whoami" "what is 2+2? answer with the number only"; do
    n=$((n + 1))
    home=$WORK/home$n
    mkdir -p "$home"
    stream=$OUT/prompt$n.jsonl
    if ! OWLPOST_HOME=$home owl init --name e2e-watch >"$OUT/prompt$n.init.log" 2>&1; then
        echo "FAIL $prompt (owl init failed, see $OUT/prompt$n.init.log)"
        failures=$((failures + 1))
        continue
    fi
    # The run: a bounded `claude -p` in the background, its stream kept as evidence.
    if [ -n "${E2E_PLUGIN_DIR:-}" ]; then
        (
            cd "$WORK" && OWLPOST_HOME=$home timeout 180 claude -p "$prompt" \
                --output-format stream-json --verbose --max-turns 6 \
                --plugin-dir "$E2E_PLUGIN_DIR" >"$stream" 2>"$OUT/prompt$n.stderr"
        ) &
    else
        (
            cd "$WORK" && OWLPOST_HOME=$home timeout 180 claude -p "$prompt" \
                --output-format stream-json --verbose --max-turns 6 \
                >"$stream" 2>"$OUT/prompt$n.stderr"
        ) &
    fi
    runner=$!
    # Wait up to 120 s for the session id and then for a marker with a live pid.
    sid=""
    marker_pid=""
    reason=""
    waited=0
    while [ $waited -lt 120 ]; do
        if [ -z "$sid" ] && [ -s "$stream" ]; then
            sid=$(session_id "$stream")
        fi
        if [ -n "$sid" ] && [ -f "$home/watch/$sid" ]; then
            marker_pid=$(tr -d '[:space:]' <"$home/watch/$sid")
            if [ -n "$marker_pid" ] && pid_alive "$marker_pid"; then
                break
            fi
            marker_pid=""
        fi
        if ! pid_alive "$runner"; then
            break
        fi
        sleep 1
        waited=$((waited + 1))
    done
    if [ -z "$sid" ]; then
        reason="no session_id in the stream"
    elif [ -z "$marker_pid" ]; then
        reason="no live marker at $home/watch/$sid within ${waited}s"
    else
        # The Monitor call in the stream: name, description and the session id on one line.
        if ! grep -F '"name":"Monitor"' "$stream" | grep -F '"description":"owlpost inbox"' | grep -Fq -- "--session $sid"; then
            reason="marker live (pid $marker_pid) but no Monitor tool_use with description \"owlpost inbox\" and --session $sid in the stream"
        fi
    fi
    kill_tree "$runner"
    [ -n "$marker_pid" ] && kill "$marker_pid" 2>/dev/null
    wait "$runner" 2>/dev/null
    if [ -z "$reason" ]; then
        echo "PASS $prompt (session $sid, marker pid $marker_pid, ${waited}s)"
    else
        echo "FAIL $prompt ($reason)"
        failures=$((failures + 1))
    fi
done

rm -rf "$WORK"
echo "streams kept under $OUT"
[ "$failures" -eq 0 ] || exit 1
exit 0
