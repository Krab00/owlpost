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

## Quickstart

Seven steps from nothing to the first answer. Steps 1–3 are once per machine, 4–5 once per
person and repository, 6–7 whenever you like.

1. **Install** the `owl` binary (macOS or Linux, x86_64 or aarch64; verifies the release
   checksum, installs to `~/.local/bin`, `--system` for `/usr/local/bin`):

   ```
   curl -fsSL https://raw.githubusercontent.com/Krab00/owlpost/main/scripts/install.sh | sh
   ```

2. **`owl init`** — creates your key and config under `~/.config/owlpost` and prints your fingerprint
   (`owl init --name "Your Name" --email you@company.com`).

3. **`owl install`** — registers the `owl daemon` as a launchd / systemd user service so
   questions and answers flow while you are away from the terminal (`--dry-run` shows the unit).

4. **`owl contact export`** — prints your peer file (name, email, fingerprint, public key,
   endpoints). Save it as `.agents/peers/<your-slug>.json` in the repository you work on.

5. **PR** that peer file into `.agents/peers/`. The directory is `CODEOWNERS`-protected; an admin
   compares the fingerprint in the PR with one you read them out-of-band before approving
   (`docs/pilot.md`).

6. **`owl doctor`** — checks key, config, endpoints, harness binaries and that the daemon is
   reachable. Every line green means you can receive questions; ask a peer to run it too.

7. **`owl ask <peer> [path] "<question>"`** — your first question. The peer approves (or has you
   on auto), their agent answers read-only from their checkout, the answer lands in your inbox
   (`owl inbox`, `owl show <id>`) and in your agent's memory via the harness plugin
   (`plugins/claude-code/README.md`).

Pilot users: `docs/pilot.md` has the onboarding checklist, the admin fingerprint procedure,
what to measure and when to stop.
