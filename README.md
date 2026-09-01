# owlpost

Ask a colleague's coding agent a question about their code — asynchronously, peer-to-peer,
with a human approving every answer that leaves their machine.

`owl ask maciek src/auth/session.rs "why is the refresh token rotated on every read?"`

The question travels over mutual-TLS to Maciek's laptop, lands in his inbox, and (after he
approves, or automatically for trusted peers) a **read-only** headless session of his own
coding agent answers from his repo checkout and project notes. The answer comes back to your
inbox and into your agent's memory. No central server, no shared memory, no bug tracker.

Works across harnesses: Claude Code first, Codex / Kimi Code / opencode next.

- [Concept](docs/concept.md) — what it is, what it is not, the decisions and why
- [Architecture](docs/architecture.md) — components, flows, trust model, protocol
- [Technical design](docs/technical-design.md) — crate layout, data formats, CLI and HTTP contracts, testing
- [Plan](docs/plan.md) — milestones and the task backlog driven by `/orch`

Status: pre-alpha, private. Rust, single static binary `owl`.
