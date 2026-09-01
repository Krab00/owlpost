---
backend: manifest
tasks_dir: .orch/tasks
default_base: main
branch_prefix: task/
capabilities:
  tests: { root: "cargo test --workspace" }
standing_checks_extra: []
---
owlpost — Rust CLI + daemon (`owl`), one crate, binary `owl`. Specs live in `docs/`:
`docs/technical-design.md` is the contract every task implements; `docs/architecture.md` the
flows; `docs/concept.md` the why. When a task and the design doc disagree, the design doc wins
and the task file must be corrected in the same PR.

Conventions the implementer must follow:
- Toolchain: Rust stable, edition 2024. Gate before reporting done: `cargo build`,
  `cargo test --workspace`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`.
- No new crates beyond the list in `docs/technical-design.md §1` without a one-line justification
  in the PR body.
- Every test uses its own `$OWLPOST_HOME` temp dir (`tempfile`) and binds `127.0.0.1:0`. Tests
  must pass offline and must never invoke a real LLM harness — use `tests/fixtures/fake-harness.sh`.
- Deliberate shortcuts carry a `// ponytail: <ceiling> — <upgrade path>` comment.
- Never touch `main` directly; every task on its own `task/<id>-<slug>` branch and worktree.

No web or api capability is declared on purpose: the daemon speaks mTLS-only HTTPS, so a generic
curl health check cannot pass; API behaviour (4xx on malformed input, never 500) is covered by
`tests/server.rs` and is an explicit acceptance criterion of OWL-006.

Manual real-harness runs (`scripts/e2e-real.sh`) are for humans, not for the verify loop.
