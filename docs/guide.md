# User guide

End to end, every command, in the order you will need them.

Running example: **Ania** asks **Bartek** a question about his repository. Ania is the asker,
Bartek is the responder. Both do steps 1 and 2 on their own machine.

Each step gives the terminal command and the Claude Code slash command. Both do the same thing;
the slash command adds a confirmation step and shows the output for you.

---

## 1. Install

### 1.1 Get the binary

```
curl -fsSL https://raw.githubusercontent.com/Krab00/owlpost/main/scripts/install.sh | sh
```

Downloads a release of `owl`, verifies its SHA-256, installs it to `~/.local/bin`.
Flags: `--version <tag>`, `--prefix <dir>`, `--system` (installs to `/usr/local/bin`), `--help`.

You should see the install line and then `owl --version` works.

### 1.2 Everything else in one command

```
owl setup --name "Ania Nowak" --email ania@company.com
/owlpost:setup --name "Ania Nowak" --email ania@company.com
```

Runs 1.3–1.5 for you: `owl init` (skipped when a key exists), `owl install`, then
`claude plugin marketplace add Krab00/owlpost` and `claude plugin install owlpost@owlpost-local`.
`--plugin-source <dir>` installs the plugin from a local checkout (`<repo>/plugins/claude-code`);
`--dry-run` lists the steps. Without `claude` on `PATH` the plugin step is skipped.
You should see your fingerprint, `installed`/`started` for the daemon, and
`set up; run \`owl doctor\``. Then skip to 1.6.

Plugin first, binary second also works: after 1.5 alone, every session starts with
`owlpost: the owl binary is not installed; run /owlpost:setup ...`, and `/owlpost:setup`
runs the installer from 1.1 before `owl setup`. The plugin steps inside `owl setup` are no-ops
for a plugin that is already installed.

### 1.3 Create your identity

```
owl init --name "Ania Nowak" --email ania@company.com
/owlpost:init --name "Ania Nowak" --email ania@company.com
```

Creates the key and config under `~/.config/owlpost`. Without `--name` / `--email` it asks
for each in turn on the terminal (`/owlpost:init` and `/owlpost:setup` ask in the session).
You should see one line: your fingerprint, `owl:` plus 16 characters.
If a key already exists, the command refuses and changes nothing.

### 1.4 Install the daemon

```
owl install
/owlpost:install
```

Registers `owl daemon` as a launchd (macOS) or systemd user (Linux) service, so questions and
answers flow while you are away from the terminal.
You should see `installed <path>` and `started <service>`.
`owl install --dry-run` prints the unit file without installing anything.

### 1.5 Install the Claude Code plugin

```
claude plugin marketplace add Krab00/owlpost
claude plugin install owlpost@owlpost-local --scope user
```

Or from a local checkout: `claude plugin marketplace add <repo>/plugins/claude-code`.
Restart the session afterwards so the hooks register.

Optional: run it as a Claude Code mod. Set `CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1` (in the shell
or under `env` in `~/.claude/settings.json`) and restart. You get an inbox band above the prompt
and a pane for contacts, asking and the inbox. `/owlpost:contacts` or `/owlpost:ask` with no
arguments opens the pane, and running the same command again closes it (so does **Close**).
Details: `plugins/claude-code/README.md`, "Claude Code mods".

### 1.6 Check the setup

```
owl doctor
/owlpost:doctor
```

One line per check: `ok|warn|fail  <name>: <detail>`, for key, config, endpoints, harnesses,
daemon, iroh and the last pull. Exits 1 if any check fails.
You should see `ok` on the key, config and daemon lines.

---

## 2. Identity and contacts

### 2.1 See who you are

```
owl whoami
/owlpost:whoami
```

Prints fingerprint, pubkey, name and endpoints.

### 2.2 Hand your peer file to a colleague

```
owl contact export
/owlpost:me
```

Prints your peer file as JSON: `name`, `emails`, `pubkey`, `endpoints`.
Ania sends this to Bartek; Bartek sends his to Ania.
For the pilot repository you save it as a file instead:
`owl contact export > .agents/peers/ania.json` and open a PR (see `docs/pilot.md`).

### 2.3 Add the colleague

```
owl add bartek.json
/owlpost:add bartek.json
```

Adds a peer file to your global book (`$OWLPOST_HOME/contacts/`). The argument is a path, the
JSON itself, or `-` to read stdin. `--local` writes to the repo's `.agents/peers/` instead.
You should see `added Bartek owl:xxxxxxxx (global)`.
Adding sets no policy. Confirm the fingerprint with Bartek by call or chat before allowing him.

### 2.4 List and inspect contacts

```
owl contact list
owl contact show bartek
/owlpost:contacts
/owlpost:contact show bartek
```

`list` prints NAME, FINGERPRINT, SOURCE (global or local) and POLICY; `--global` or `--local`
restricts the scope. `show` prints one contact as JSON.
`/owlpost:contacts` shows a picker you navigate with the arrow keys.
With Claude Code mods on (§1.5) it opens the owlpost pane instead; run it again to close it.

### 2.5 Remove a contact

```
owl contact remove bartek
/owlpost:contact remove bartek
```

Deletes the contact file from the global book; `--local` removes it from `.agents/peers/`.
You should see `removed Bartek (global)`.

### 2.6 Cards

```
owl card
owl card bartek
/owlpost:card
/owlpost:card bartek
```

Own card with no argument (the running daemon's copy, which also lists the iroh interface),
the peer's card with one (fetched from the first reachable endpoint). The card is an A2A 1.0
agent card: who this is, how to authenticate (`owl-mtls`), the one skill (`ask-about-repo`)
and the three owlpost extensions (identity, repository question, human gate).

---

## 3. Ask, receive, consent, answer

### 3.1 Ania asks

```
owl ask bartek src/auth/session.rs "why is the refresh token rotated on every read?"
/owlpost:ask bartek src/auth/session.rs why is the refresh token rotated on every read?
```

Sends the question to Bartek's machine. The path is optional; without it the question is about
the whole repository.
You should see `accepted <id> — waiting for the owner's consent` (or `— the owner's agent is
answering` once Bartek has allowed you), or the answer text right away when it came from the
cache or Bartek has you on auto.

Other flags:
- `--wait <secs>` — block and poll for the answer; every change on Bartek's side prints one
  line `<HH:MM> <state>` (`waiting for the owner's consent`, `the owner's agent is
  answering`, `the owner is reviewing the answer`); `declined by Bartek: the owner declined`
  ends the wait with exit code 2; exit code 4 on timeout.
- `--reply-to <id>` — continue an earlier exchange with Bartek (the id of a question you sent
  or of an answer you received): his agent then sees the earlier questions and answers of
  that thread. An unknown id is `no exchange <id>`, another peer's is `<id> was asked to
  <name>, not <peer>`.
- `--context <file>` — attach a snippet (a diff, an error, a file excerpt; `-` reads stdin;
  at most 8192 bytes, else `context is <n> bytes, max 8192`) so Bartek's agent answers the
  question actually being asked. Shown under the question in `owl show`.
- `--file <path>` — propose peers from `git blame` of that file; the path is also the question's path.
- `--peer <peer>` — name the peer instead of picking one.
- `--project <id>` — override the detected project id.
- `--no-cache` — skip the local answer cache (a question with `--context` or `--reply-to`
  skips it anyway).

Failures: `offline` (no endpoint reachable, exit 2), `unavailable` (policy never, exit 2),
`rate limited` (exit 3).

### 3.1a Where does it stand?

```
owl status
owl status <id>
/owlpost:status
```

One row per open question: `ID  PEER  PATH  STATE  SINCE`, the STATE in Bartek's words
(`waiting for the owner's consent`, `the owner's agent is answering`, `the owner is reviewing
the answer`), `offline` when his machine cannot be reached right now. A question Bartek
declined moves to history as `declined`. `--json` prints the raw task objects. Exit code 4
with `no open questions` when nothing is open.

### 3.2 Bartek sees the question

```
owl inbox
/owlpost:inbox
```

Table of ID, FROM, TYPE, STATE, PATH, AGE. Listing marks records seen.
A held question shows a line under the table:
`Ania wants to ask your agent about <project> — owl allow <fp> [--once|--always] / owl deny <fp>`.
`owl inbox --new` lists only unseen records; `owl inbox --count` prints the unseen count without
marking anything.

### 3.3 Read one record

```
owl show <id>
/owlpost:show <id>
```

Prints id, from, type, state, received, project, path and the full question (plus the draft when
there is one). Marks the record seen. `owl show all` prints every inbox record.

### 3.4 Watch for new mail

```
owl watch
/owlpost:watch on
```

`owl watch` blocks until an unseen record arrives, prints its id and exits; `--id <id>` waits for
one record, `--timeout <secs>` gives up with exit code 4.
`/owlpost:watch on|off|status` is the session version: a background poll that prints one line
when the inbox count changes. `off` stops it, `status` reports it.

### 3.5 Consent

```
owl allow ania
/owlpost:allow ania
```

Releases Ania's held questions and sets policy `manual` (you approve each answer).
You should see `allowed Ania (owl:xxxxxxxx): policy manual, released 1 held question`.

```
owl allow ania --once
owl allow ania --always
owl deny ania
/owlpost:allow ania --once
/owlpost:allow ania --always
/owlpost:deny ania
```

- `--once` releases the held questions and writes no policy; the next question is held again.
- `--always` sets policy `auto`: future questions are answered without asking. A hand-added
  (global) contact also needs `--i-verified-the-fingerprint`, which you pass only after checking
  the fingerprint out of band.
- `owl deny` sets policy `never`: held questions are denied, new ones get 403.
  You should see `denied Ania (owl:xxxxxxxx): policy never, 1 held question moved to done`.

### 3.6 Draft the answer

```
owl draft <id>
/owlpost:draft <id>
```

`owl draft <id>` runs the responder harness read-only against this checkout and stores the
draft on the record. You should see the draft text, then `harness: ...`, `redactions: N`,
`state: drafted (<id>)`. `--harness <name>` picks another configured harness. Exit 1 means the
draft was stored but needs a look (timeout or extraction failure).

`/owlpost:draft <id>` answers in the session instead: `owl draft <id> --prompt` prints the
responder prompt, the plugin hands it to the Agent tool (a read-only subagent on a cheaper
model), and `owl draft <id> --agent --text <reply>` stores the answer, redacted, as
`harness: agent`. `/owlpost:draft <id> --harness <name>` uses the headless harness as above.

### 3.7 Edit the draft

```
owl edit <id>
/owlpost:edit <id>
```

Opens the draft in `$EDITOR` and stores the result; you should see the new text and
`draft updated (<id>)`. `$EDITOR` cannot run inside a Claude Code session, so
`/owlpost:edit` tells you to run it in a terminal instead.

### 3.8 Send it

```
owl send <id>
/owlpost:send <id>
```

Signs the draft, moves it to the outbox and the question to `done/`.
You should see `sent <answer id> (reply to <id>, to <fingerprint>)`.

### 3.9 Or reject it

```
owl reject <id>
/owlpost:reject <id>
```

Discards the record; Ania gets no answer. You should see `rejected <id>`.

### 3.10 Ania reads the answer

The answer lands in Ania's inbox as a record of type `answer`:

```
owl inbox
owl show <id>
/owlpost:inbox
/owlpost:show <id>
```

---

## 4. History

```
owl history
/owlpost:history
```

Finished exchanges: ID, PEER, TYPE, STATE, PATH, RECEIVED.

Filters:

```
owl history --peer bartek
owl history --path "src/auth/*.rs"
owl history --since 3d
```

- `--peer` matches the fingerprint exactly or the contact name, case-insensitive.
- `--path` is a glob with `*` and `?` only.
- `--since` takes RFC3339 (`2026-09-01T10:00:00Z`) or `<N>d`, `<N>h`, `<N>m`.

Full content of one exchange:

```
owl show <id>
```

Check history before asking a question that may already be answered.

---

## 5. Maintenance

### 5.1 Update

```
owl update
/owlpost:update
```

Replaces the `owl` binary, re-registers the daemon and reinstalls the Claude Code plugin.
You should see `updated; restart your Claude Code session to load the plugin`.
`--source <dir>` builds from a local checkout instead of downloading a release;
`--dry-run` prints the commands it would run.

### 5.2 Uninstall the service

```
owl uninstall
/owlpost:uninstall
```

Stops and removes the daemon service; questions and answers stop flowing until you run
`owl install` again. You should see `stopped <service>` and `removed <path>`.

---

## Global flags

Every command accepts:

- `--home <dir>` — use another owlpost home (overrides `$OWLPOST_HOME`).
- `--json` — machine-readable output.
- `-q`, `--quiet` — suppress non-essential output.

Exit codes: 0 ok, 1 user or data error, 2 offline or unavailable, 3 rate limited,
4 nothing to do (a `watch` or `--wait` timeout).
