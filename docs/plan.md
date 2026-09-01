# owlpost — plan

Milestones map 1:1 onto the task files in `.orch/tasks/` (driven by `/orch`). Each task has
tool-checkable acceptance criteria; `deps` enforce the order. Run `/orch all` (or
`/loop /orch all`) to drain the backlog in dependency order, `/orch batch 3` for independent
tasks in parallel.

Prerequisite on the machine running `/orch`: Rust stable toolchain (`rustup`), `gh` CLI
authenticated, and for the real-harness e2e script at least one of `claude`, `codex`,
`opencode` on `PATH`.

## M0 — Foundation (no network)

| Task | Deliverable |
|---|---|
| OWL-001 | Cargo project, `owl` binary with clap skeleton, config loading, CI (build/test/clippy/fmt) |
| OWL-002 | Identity: keypair generation, fingerprint, sign/verify, `owl init`, `owl whoami` |
| OWL-003 | Envelope schema and spool state machine with atomic writes |
| OWL-004 | Contact providers `repo` + `local`, policy overlay, `owl contact export` |

Exit: `cargo test` green; `owl whoami` prints a fingerprint; a question can be created, signed,
stored and moved through spool states in a test.

## M1 — Network

| Task | Deliverable |
|---|---|
| OWL-005 | mTLS spike: self-signed cert from ed25519 key, pinned verifiers, two-config localhost test |
| OWL-006 | Daemon HTTP API: agent card, `POST /v1/questions`, `GET /v1/outbox`, ack, rate limit, replay |
| OWL-007 | `owl ask`: contact resolution (name/email/fingerprint/blame), cache, send, `--wait` |
| OWL-008 | Pull loop in the daemon, answer ingestion, ack, liveness probe cache |

Exit: two daemons on localhost with distinct homes exchange a signed question and a signed
answer end to end in an integration test, with an unknown key refused at handshake.

## M2 — Responder and inbox

| Task | Deliverable |
|---|---|
| OWL-009 | Runner: harness templates (claude / codex / opencode / kimi-disabled), prompt build, redaction, fake harness for tests |
| OWL-010 | Inbox CLI: `inbox`, `show`, `draft`, `send`, `reject`, `edit`, `history`, seen semantics, `--format` for hooks |
| OWL-011 | Consent and policy: hold-for-consent, `allow`/`deny`, `never` = unavailable, auto-accept path with outgoing log |
| OWL-012 | OS notifications, `owl install`/`uninstall` (launchd/systemd), `owl watch` |

Exit: full manual loop works from a plain terminal with the fake harness; auto-accept loop
works with redaction and logging; a real-harness run is documented and scripted.

## M3 — Claude Code adapter and release

| Task | Deliverable |
|---|---|
| OWL-013 | Claude Code plugin: hooks, skill, slash commands, memory-write guidance |
| OWL-014 | End-to-end test suite: two daemons + fake harness + hook output; `scripts/e2e-real.sh` for real harnesses |
| OWL-015 | Release pipeline: multi-platform builds, `install.sh`, README quickstart, pilot onboarding doc |

Exit: a colleague installs with one command, appears in `.agents/peers/` via PR, and answers a
question from Claude Code without leaving the session.

## M4 — Post-MVP (not yet tasked)

- Codex, Kimi Code and opencode adapters (configuration packages; Kimi responder after the
  fail-closed test).
- Peer-visible answer cache; repeated-question → `AGENTS.md` PR proposal.
- Lesson / bug-report message type with proposed memory rule.
- Signed peer-file changes + CI check for the `repo` provider.
- Statusline badge where the harness supports it.
- Invite bundle / QR for unreachable hosts.

## Pilot (after M3)

4–5 people, 2 weeks. Measure: questions asked, answered, median latency manual vs auto-accept,
share of answers the asker rated better than what their agent could derive from git history,
opt-in rate for private notes. Kill criteria: no measurable delta after 2 weeks.
