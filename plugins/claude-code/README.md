# owlpost plugin for Claude Code

The Claude Code harness adapter for owlpost (`docs/concept.md` "Harness adapters",
`docs/architecture.md` §3.3 and §3.6). It ships:

| Piece | Path | What it does |
|---|---|---|
| Hooks | `hooks/hooks.json`, `hooks/owl-count.sh` | On `SessionStart`, `UserPromptSubmit` and `PostToolUse` runs `owl inbox --count --format claude` (5 s timeout) and injects the unseen-question counter line. Silent no-op when `owl` is missing or fails. |
| Skill | `skills/owlpost/SKILL.md` | When to ask a peer, how to run `owl ask`, how to react to the counter, the answer loop, the memory rule. |
| Commands | `commands/<name>.md`, one per `owl` subcommand (all except `daemon`) | `/owlpost:<name>` runs `owl <name>` with the arguments and offers the next step; see the table below. |
| Manifests | `.claude-plugin/plugin.json`, `.claude-plugin/marketplace.json` | Plugin metadata; a one-plugin marketplace so the directory can be added from a local path. |

## Commands

Every `owl` subcommand except `daemon` (a service: `install`, `uninstall` and `doctor`
cover it) is one `commands/<name>.md`. Each is a thin wrapper: run the command with the
arguments, show the output, offer the natural next step. `send`, `allow`, `deny`, `reject`
and `uninstall` ask for one explicit confirmation with `AskUserQuestion` before running.
`allowed-tools` in each file is limited to its own `owl <sub>:*` pattern (plus the `owl`
patterns of the next steps a file runs itself); `tests/plugin.rs` pins the rule.

| Command | File | Runs |
|---|---|---|
| `/owlpost:init [--name <name>] [--email <email>]` | `commands/init.md` | `owl init` — create home, key, config |
| `/owlpost:whoami` | `commands/whoami.md` | `owl whoami` — identity summary |
| `/owlpost:me` | `commands/me.md` | `owl contact export` — own peer file to hand to a colleague |
| `/owlpost:card [peer]` | `commands/card.md` | `owl card` — own card, or fetch a peer's |
| `/owlpost:contacts` | `commands/contacts.md` | `owl contact list --json` — pick a peer with the arrow keys, then ask or show the card |
| `/owlpost:contact <show\|export\|remove> ...` | `commands/contact.md` | `owl contact show|export|remove` |
| `/owlpost:add <peer json\|file> [--local]` | `commands/add.md` | `owl add` — add a peer file |
| `/owlpost:allow <peer> [--once\|--always]` | `commands/allow.md` | `owl allow` — release held questions, set policy (confirms first) |
| `/owlpost:deny <peer>` | `commands/deny.md` | `owl deny` — policy never (confirms first) |
| `/owlpost:ask <peer> [path] <question>` | `commands/ask.md` | `owl ask` — send a question (confirms first) |
| `/owlpost:inbox` | `commands/inbox.md` | `owl inbox` — list records, offer the next action |
| `/owlpost:show <id>` | `commands/show.md` | `owl show` — full record, offer draft/send/reject |
| `/owlpost:draft <id> [--harness <name>]` | `commands/draft.md` | `owl draft` — run the responder, show the draft |
| `/owlpost:edit <id>` | `commands/edit.md` | `owl edit` opens `$EDITOR`, which cannot run in a session; explains the alternatives |
| `/owlpost:send <id>` | `commands/send.md` | `owl send` — sign and move to outbox (confirms first) |
| `/owlpost:reject <id>` | `commands/reject.md` | `owl reject` — discard a record (confirms first) |
| `/owlpost:history [--peer] [--path] [--since]` | `commands/history.md` | `owl history` — finished exchanges |
| `/owlpost:watch [--id <id>] [--timeout <secs>]` | `commands/watch.md` | `owl watch` — block until a record arrives (OWL-023 replaces it) |
| `/owlpost:install [--dry-run]` | `commands/install.md` | `owl install` — register the daemon service |
| `/owlpost:uninstall` | `commands/uninstall.md` | `owl uninstall` — remove the service (confirms first) |
| `/owlpost:doctor` | `commands/doctor.md` | `owl doctor` — check the setup, offer the fix per failure |
| `/owlpost:update [--source <dir>]` | `commands/update.md` | `owl update` — binary, daemon, plugin |

## Requirements

- `owl` on `PATH` (`cargo install --path .` from the repo root, or the release binary).
  The hook prints nothing when `owl` is not found, so the plugin loads without it but the
  counter stays silent.
- An initialised home: `owl init`, then contacts via `owl add <peer-file>` (global book;
  `--local` for the repo's `.agents/peers/`) and `owl allow`.
- The `owl daemon` running as a service so questions and answers actually flow:
  `owl install` registers the launchd/systemd unit (`owl install --dry-run` shows what it
  would write). Check with `owl doctor`.

## Install

From a local checkout:

```
/plugin marketplace add /absolute/path/to/owlpost/plugins/claude-code
/plugin install owlpost@owlpost-local
```

From the repository (once published):

```
/plugin marketplace add Krab00/owlpost
/plugin install owlpost@owlpost-local
```

`owlpost-local` is the marketplace name in `.claude-plugin/marketplace.json`; the plugin
`source` there is `./`, so the plugin directory is its own marketplace. Restart the session
(or `/plugin` → reload) after installing so the hooks register.

The plugin version in `.claude-plugin/plugin.json` is copied by hand from `Cargo.toml`;
`tests/plugin.rs` fails when they drift.

## Manual smoke run (AC5, PM-REVIEW)

Automated tests cover the hook script (`cargo test --test plugin`). Whether Claude Code
actually shows the injected line has to be observed once by hand:

1. Build and put `owl` on `PATH`: `cargo install --path .` (or symlink `target/debug/owl`).
2. Use a throwaway home so the real inbox is untouched, and seed it with the fixture the
   integration tests use (it writes key, config, one contact "Maciek" and two unseen
   questions from Maciek, then prints the expected counter line):

   ```
   export OWLPOST_HOME=$(mktemp -d)
   cargo test --test plugin seed_home_for_smoke -- --ignored --nocapture
   owl inbox --count --format claude     # must print one JSON line
   ```

3. Keep `OWLPOST_HOME` exported in the shell that starts Claude Code; the hook inherits it.
4. Install the plugin from the local path (commands above) in a Claude Code session started
   with the same `OWLPOST_HOME` exported.
5. Start a new session (`claude`) in any directory and send any prompt. The `SessionStart`
   and `UserPromptSubmit` hooks fire; the counter line
   `owlpost: 2 new questions (Maciek 2). Say "show owlpost inbox" or run `owl inbox`.`
   must appear in the injected context (visible in the transcript with `Ctrl+O`, or ask
   Claude "what did the owlpost hook inject?").
6. Say "show owlpost inbox"; Claude should run `owl inbox` and list the two rows.
7. Paste the observed output into the block below and into `.orch/tasks/OWL-013.md` under
   AC5.

### Observed output

```
<paste the hook line / transcript excerpt here, with the date and the Claude Code version>
```
