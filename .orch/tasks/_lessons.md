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

## OWL-004 (2026-09-02)
Round 1 caught a real panic: `set_policy` indexed `v["policy"]` on a serde_json::Value before checking it was an object. Rule: any code that indexes into a parsed JSON Value must go through `as_object_mut()`/`get()` first, and every file-reading path needs a test with wrong-shape (not just invalid) JSON. The 11 test gaps were again tier/ordering "negative twins" (email-before-fingerprint, prefix on emails, `.git` as file).

## OWL-009 (2026-09-02)
Round 1 failed only on AC7: the AC-named unit test re-implemented the production `Err => (raw, ExtractFailed)` arm inside the test and asserted on its own value, so mutants of the real branch survived it (the integration twin killed them, but the named test must prove its own criterion). Fix pattern: extract fallbacks into a small production fn and have the named test call it AND drive the public entry point. Verifier lesson: run mutant sweeps as `cargo test --lib <mod>` and `cargo test --test <bin>` separately — a positional filter silently drops integration binaries and yields false survivors (14 here).

## OWL-006 (2026-09-02)
Code correct in round 1; five ACs lost on 7 mutant survivors, all untested matrix rows: a config value read by the handler but only tested at its default (card harness, global rate limit), restart persistence never exercised (SeenIds reload), a boolean x policy matrix with one row (cache x policy), check ordering (rate limit before policy), and a "stray" fixture that did not differ from the positive one in the filtered dimension (outbox kind). Two checks would have caught all seven: for every config value a handler reads, one test with a non-default value; every negative fixture must differ from its positive twin in exactly the filtered dimension. Also a real spec gap surfaced: the task file put cache before consent while architecture §3.2 puts policy first — resolved for §3.2 and the task file corrected on the branch.

## OWL-012 (2026-09-02)
Code correct in round 1; lost on four untested arms (a config value tested only at its default — pull interval threshold; an `Option::None` control-flow arm — watch without --timeout; two fallback arms nobody drove — path "?" and empty contact name) plus a new kind of defect: a feature with a host-visible side effect (notify) landed without neutralising it in the shared daemon fixture, so every existing server test fired a real desktop popup. Rules: every `None`/fallback arm gets a test; any side-effecting feature must default OFF in `tests/common` the moment it lands and one test must assert the fixture default.

## OWL-010 (2026-09-02)
Round 1 found the recurring IndexMut-on-Value panic (reject/send indexed `meta` after already moving the record to done/), round 2 fixed it by reordering I/O in a shared `finish()` helper — and shipped the reorder with a comment ("a retry redoes") that no test checked; blocking the rename with a directory shows `send` cannot be retried at all and wedges the record. Rule: any commit that reorders I/O steps for atomicity must ship the failure-path test (block the destination with a directory, per OWL-003) for every caller and assert the retry outcome; recovery claims belong in tests, not comments. Third recurrence of "Value indexing" (OWL-004, OWL-006 lenient outbox, OWL-010) → promote to standing_checks_extra.

## OWL-007 (2026-09-02)
Code correct in both rounds; round 1 lost on six untested paths, round 2 fixed all six but lost on the same shape of gap: an "ordered by X" assertion whose fixture sorts identically under X and under the tiebreak (Ana before Bea by name and by share), and fallback arms the daemon fixture can never induce (Retry-After absent, unreadable cache record). Rule: any "ordered by X" test must use a fixture where X-order differs from every other plausible sort key (name, insertion); once a scripted fake exists, every response-shape arm in the client (missing header, missing field) gets one scripted row. Verifier note: a sweep script that greps `error:` misclassifies `error: test failed` as NOCOMPILE — classify on `error[E` or rc only.

## OWL-007 — fresh run round 1 (2026-09-02)
Tests-only round closed all three gaps on the first try once the fix shape was spelled out (share order differing from every ascending key; one scripted-fake row per response-shape arm), so a tests-only fix loop converges in one round when the escalation names exact fixtures. Remaining weak shape the rules do not cover: a timing fixture whose fake records no counters, so "warned once for two failures" is asserted client-side only and depends on machine load — fakes should count what they served and tests should assert on the fake's counter.
