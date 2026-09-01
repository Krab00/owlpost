---
id: _lessons
title: Retro log (not a task)
status: done
deps: []
---
# Lessons

Append 2–3 sentences per resolved task: what failed, why, which check would have caught it.

## OWL-001 (2026-09-02)
Round 1 failed only on test gaps, not behaviour: a `starts_with` help assertion let superstring renames survive, the `whoami` guard's two halves (missing config vs missing key) had one shared test, and `Config::default()` literals from §3 were not pinned so value drift went unnoticed. Check that catches it: the verifier's independent mutant sweep (rename a token, flip `||`→`&&`, edit a default string) — implementers should assert exact tokens/values, and test each half of a compound guard separately. Also: the implementer's own sweep gave false survivors because it reverted an uncommitted test file mid-sweep — commit before sweeping.

## OWL-002 (2026-09-02)
Same pattern as OWL-001: code correct, round 1 failed on 4 untested branches (33-byte key accepted by a truncating read, verify() ignoring the pubkey, parse_* falling back to bare base64, endpoints joiner). Each was a "negative twin" of an existing positive assertion — for every parser/validator/verifier, add the reject case for each input dimension (wrong length above AND below, wrong key, missing prefix), and a non-empty-collection case for every formatter. Recurring across 2 tasks → candidate for `standing_checks_extra`.

## OWL-003 (2026-09-02)
Round 1 found two real bugs, both in hand-rolled I/O edges: a permissive RFC3339 parser (range checks only, so `-1` and `+2026` slipped through) and an atomic-write helper that left `<id>.json.tmp` when rename failed. Fix pattern: hand-written parsers must be shape-strict (fixed width, digits only) with a reject case per field, and every temp-file write needs a failure-path test (block the destination with a directory). Test gaps again matched the "negative twin" lesson (verify-before-parse, exact prune boundary).

## OWL-005 (2026-09-02)
Code correct from round 1; three rounds spent purely on test coverage. Round 2 closed six negative cases, round 3 the missing POSITIVE twin (unpinned mode must still identify a known client) — the reviewer and the mutant sweep hit it independently. Lesson: for every boolean mode flag on a verifier, test the 2×2 matrix (flag on/off × input known/unknown), not just the corners the AC names. Recurs with OWL-002/003/004's "negative twin" pattern → promote a test-matrix rule into standing_checks_extra.
