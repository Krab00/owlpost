# owlpost plugin for Claude Code

The Claude Code harness adapter for owlpost (`docs/concept.md` "Harness adapters",
`docs/architecture.md` §3.3 and §3.6). It ships:

| Piece | Path | What it does |
|---|---|---|
| Hooks | `hooks/hooks.json`, `hooks/owl-count.sh` | On `SessionStart`, `UserPromptSubmit` and `PostToolUse` runs `owl inbox --count --format claude` (5 s timeout) and injects the unseen-question counter line; `SessionStart` also registers this session's private wake directory `$OWLPOST_HOME/sessions/<session_id>/wake` as a `watchPaths` entry, and the `FileChanged` hook (`asyncRewake`) wakes the idle session — exit 2 with the framed message block — when the daemon routes a record to it (one session per record); `SessionEnd` removes the directory. Silent no-op when `owl` is missing or fails. |
| Skill | `skills/owlpost/SKILL.md` | When to ask a peer, how to run `owl ask`, the mentioned contact, how to react to the counter, the answer loop, the memory rule. |
| MCP server | `owl mcp`, registered at user scope by `owl setup` / `owl update` | Serves every contact as an `@owl:to://…` resource in the `@` typeahead (stdio); see "Mention a contact" below. |
| Commands | `commands/<name>.md`, one per `owl` subcommand (all except `daemon`) | `/owlpost:<name>` runs `owl <name>` with the arguments and offers the next step; see the table below. |
| Manifests | `.claude-plugin/plugin.json`, `.claude-plugin/marketplace.json` | Plugin metadata; a one-plugin marketplace so the directory can be added from a local path. |

## Commands

Every `owl` subcommand except `daemon` (a service: `install`, `uninstall` and `doctor`
cover it) and `mcp` (started by Claude Code, not by hand) is one `commands/<name>.md`. Each is a thin wrapper: run the command with the
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
| `/owlpost:contacts` | `commands/contacts.md` | `owl contact list --json` — the book as one table, then the hint to mention a contact with `@owl:to://…` |
| `/owlpost:contact <show\|export\|remove> ...` | `commands/contact.md` | `owl contact show|export|remove` |
| `/owlpost:add <peer json\|file> [--local]` | `commands/add.md` | `owl add` — add a peer file |
| `/owlpost:allow <peer> [--once\|--always]` | `commands/allow.md` | `owl allow` — release held questions, set policy (confirms first) |
| `/owlpost:deny <peer>` | `commands/deny.md` | `owl deny` — policy never (confirms first) |
| `/owlpost:ask <peer> [path] <question>` | `commands/ask.md` | `owl ask` — send a question at once; the command is the approval, no confirmation step; `--reply-to <id>` continues a thread, `--context <path>` attaches a snippet |
| `/owlpost:status [id]` | `commands/status.md` | `owl status` — where every open question stands, in the peer's words (`waiting for the owner's consent`, …) |
| `/owlpost:inbox` | `commands/inbox.md` | `owl inbox --json` — walk the records: question verbatim, consent picker (allow once/always, deny), draft / draft & send / reject / skip picker, verbatim draft, send/edit/reject picker |
| `/owlpost:show <id>` | `commands/show.md` | `owl show` — full record, offer draft/send/reject |
| `/owlpost:draft <id> [--harness <name>] [--send]` | `commands/draft.md` | `owl draft` — run the responder, show the draft; `--send` then runs `owl send` at once |
| `/owlpost:edit <id>` | `commands/edit.md` | `owl edit` opens `$EDITOR`, which cannot run in a session; explains the alternatives |
| `/owlpost:send <id>` | `commands/send.md` | `owl send` — sign and move to outbox (confirms first) |
| `/owlpost:reject <id>` | `commands/reject.md` | `owl reject` — discard a record (confirms first) |
| `/owlpost:history [--peer] [--path] [--since]` | `commands/history.md` | `owl history` — finished exchanges |
| `/owlpost:watch [on\|off\|status]` | `commands/watch.md` | event-driven inbox watch, nothing to arm: the `SessionStart` hook registers this session's wake directory (`$OWLPOST_HOME/sessions/<session_id>/wake`) as a watch path and the `FileChanged` hook wakes the session with the framed message block when the daemon routes a record to it; `off` stores `{"watch": false}` in `$OWLPOST_HOME/plugin.json` (the hook stops waking at once, new sessions do not watch), `on` restores it from the next session start, `status` reports the stored default |
| `/owlpost:setup [--name] [--email] [--plugin-source] [--dry-run]` | `commands/setup.md` | `owl setup` — init, daemon, plugin in one go |
| `/owlpost:install [--dry-run]` | `commands/install.md` | `owl install` — register the daemon service |
| `/owlpost:uninstall` | `commands/uninstall.md` | `owl uninstall` — remove the service (confirms first) |
| `/owlpost:doctor` | `commands/doctor.md` | `owl doctor` — check the setup, offer the fix per failure |
| `/owlpost:update [--source <dir>]` | `commands/update.md` | `owl update` — replaces the running binary in place, daemon, plugin |

## Mention a contact

Type `@owl:` and the start of the contact's name (or e-mail, or domain) in the prompt, for
example `@owl:krz`: the `owl` MCP server lists every contact of the merged book as a
resource `to://<name-slug>.<email>`, so the typeahead ranks `owl:to://krzysztof-…` near the
top; `@owl:` alone lists the whole book. Pick with the arrows and type the question, for
example `@owl:to://ana-kowalska.ana@acme.pl src/auth/session.rs why does this retry?`; the
skill runs `owl ask --peer <fingerprint>` at once, the mention is the approval. (`@krz`
without the `owl:` prefix never beats files and connectors: that ranking is Claude Code's.)

The server is registered in Claude Code at user scope by `owl setup` and `owl update`
(idempotent: `mcp server owl already registered` when it is there), or by hand with
`claude mcp add --scope user owl -- owl mcp`. It runs `owl mcp` over stdio, so it needs
`owl` on `PATH` like the hooks; it is resources-only and adds no tools to the model
context. `owl doctor` reports it as the `mcp` check (`warn` with the `claude mcp add` line
when it is missing). The plugin bundles no server of its own: one bundled through a plugin
is named `plugin:owlpost:owl`, and that prefix sinks the contacts in the typeahead ranking.

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

In one go (also creates the identity, installs the daemon and registers the `owl` MCP
server): `owl setup`, or
`owl setup --plugin-source /absolute/path/to/owlpost/plugins/claude-code` from a checkout.

By hand from a local checkout:

```
/plugin marketplace add /absolute/path/to/owlpost/plugins/claude-code
/plugin install owlpost@owlpost-local
```

From the repository (`.claude-plugin/marketplace.json` at the repo root points at this directory):

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
