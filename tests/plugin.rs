//! OWL-013 Claude Code plugin tests: the manifests parse and register the right hooks, the
//! hook script prints exactly the §9 injection line or nothing, and the skill and command
//! files carry the strings the plugin contract requires (AC1–AC4).

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::{Peer, id, policy, prepare_home, signed};
use owlpost::contacts::Mode;
use owlpost::envelope::{self, Body, Payload};
use owlpost::identity::Identity;
use owlpost::spool::{Dir, Record, Spool};
use serde_json::{Value, json};
use tempfile::TempDir;

const OWL: &str = env!("CARGO_BIN_EXE_owl");
const PLUGIN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/plugins/claude-code");
const CLAUDE_TWO: &str = r#"{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"owlpost: 2 new questions (Maciek 2). Say \"show owlpost inbox\" or run `owl inbox`."}}"#;
const EVENTS: [&str; 3] = ["SessionStart", "UserPromptSubmit", "PostToolUse"];
/// OWL-023 AC1 literals: the arm sentence, alone and after the two-question counter.
const ARM: &str = "owlpost: arm the inbox watch (see /owlpost:watch)";
const CLAUDE_ARM_ZERO: &str = r#"{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"owlpost: arm the inbox watch (see /owlpost:watch)"}}"#;
const CLAUDE_ARM_TWO: &str = r#"{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"owlpost: 2 new questions (Maciek 2). Say \"show owlpost inbox\" or run `owl inbox`. owlpost: arm the inbox watch (see /owlpost:watch)\n- Maciek question [pending] on src/auth/session.rs: why does session 0 retry?\n- Maciek question [pending] on src/auth/session.rs: why does session 1 retry?\nowlpost: run /owlpost:inbox now."}}"#;

fn plugin(rel: &str) -> PathBuf {
    Path::new(PLUGIN).join(rel)
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(plugin(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn json(rel: &str) -> Value {
    serde_json::from_str(&read(rel)).unwrap_or_else(|e| panic!("{rel} is not JSON: {e}"))
}

/// `(frontmatter, body)` of a Markdown file with a leading `---` block.
fn frontmatter(rel: &str) -> (String, String) {
    let text = read(rel);
    let rest = text
        .strip_prefix("---\n")
        .unwrap_or_else(|| panic!("{rel} does not start with a frontmatter block"));
    let (fm, body) = rest
        .split_once("\n---\n")
        .unwrap_or_else(|| panic!("{rel} frontmatter is not closed"));
    (fm.to_string(), body.to_string())
}

/// Value of a `key:` line in a frontmatter block.
fn fm_value<'a>(fm: &'a str, key: &str) -> Option<&'a str> {
    fm.lines()
        .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix(':')))
        .map(str::trim)
}

/// Home for `me` with the contact "Maciek" and `n` questions from Maciek, `seen` as given.
fn home_with(n: usize, seen: bool) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let (me, maciek) = (id(2), id(1));
    seed(dir.path(), &me, &maciek, n, seen);
    dir
}

fn seed(home: &Path, me: &Identity, maciek: &Identity, n: usize, seen: bool) {
    prepare_home(
        home,
        me,
        false,
        &[Peer::new(
            maciek,
            "Maciek",
            Some(policy(Mode::Manual, None)),
        )],
    );
    let spool = Spool::new(home).unwrap();
    for i in 0..n {
        let env = signed(maciek, me, &format!("why does session {i} retry?"));
        let p: Payload = serde_json::from_str(&env.raw).unwrap();
        let Body::Question {
            project,
            path,
            question,
        } = &p.body
        else {
            unreachable!("signed() builds questions")
        };
        let rec = Record {
            raw: env.raw.clone(),
            sig: env.sig.clone(),
            state: "pending".into(),
            seen,
            received_at: envelope::rfc3339_now(),
            draft: None,
            meta: json!({ "peer": p.from, "hash": envelope::question_hash(project, path.as_deref(), question) }),
        };
        spool.put(Dir::Inbox, &p.id, &rec).unwrap();
    }
}

/// A directory to serve as the whole `PATH`: empty, or holding a symlink to the built `owl`.
fn path_dir(with_owl: bool) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    if with_owl {
        std::os::unix::fs::symlink(OWL, dir.path().join("owl")).unwrap();
    }
    dir
}

/// Runs `sh hooks/owl-count.sh` with `PATH` set to exactly `path` and `OWLPOST_HOME` to `home`.
fn run_hook(path: &Path, home: &Path) -> Output {
    run_hook_args(path, home, &[])
}

/// [`run_hook`] with the script's arguments, the way `hooks.json` passes `--session-start`.
fn run_hook_args(path: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new("/bin/sh")
        .arg(plugin("hooks/owl-count.sh"))
        .args(args)
        .env_clear()
        .env("PATH", path)
        .env("OWLPOST_HOME", home)
        .output()
        .unwrap()
}

fn assert_silent(out: &Output, what: &str) {
    assert_eq!(out.status.code(), Some(0), "{what}: exit {:?}", out.status);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "", "{what}: stdout");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "", "{what}: stderr");
}

// ---------- AC2 ----------

#[test]
fn hook_script_emits_context_or_nothing() {
    let with_owl = path_dir(true);
    let owl = with_owl.path();
    // The built binary is reachable through the constructed PATH and nowhere else.
    assert!(owl.join("owl").exists());

    // 2 unseen questions → exactly the §9 claude line, one trailing newline, exit 0.
    let two = home_with(2, false);
    let out = run_hook(owl, two.path());
    assert_eq!(out.status.code(), Some(0), "2 unseen: {:?}", out.status);
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!("{CLAUDE_TWO}\n"),
        "2 unseen: stdout"
    );
    assert_eq!(String::from_utf8_lossy(&out.stderr), "", "2 unseen: stderr");
    // Counting never marks anything seen.
    let spool = Spool::new(two.path()).unwrap();
    let unseen = spool.list(Dir::Inbox, |r| !r.seen).unwrap();
    assert_eq!(unseen.len(), 2, "hook must not mark records seen");

    // The same two records, already seen → nothing at all.
    let seen = home_with(2, true);
    assert_silent(&run_hook(owl, seen.path()), "2 seen");

    // No records → nothing at all.
    let empty = home_with(0, false);
    assert_silent(&run_hook(owl, empty.path()), "0 records");

    // `owl` not on PATH (the PATH is one empty directory) → nothing, exit 0.
    let without_owl = path_dir(false);
    assert!(
        std::fs::read_dir(without_owl.path())
            .unwrap()
            .next()
            .is_none()
    );
    assert_silent(&run_hook(without_owl.path(), two.path()), "no owl on PATH");
}

/// OWL-023 AC1/AC2: the SessionStart invocation (`--session-start` forwarded by the script)
/// carries the arm sentence unless `plugin.json` switches the watch off.
#[test]
fn hook_script_forwards_session_start_and_reads_plugin_json() {
    let with_owl = path_dir(true);
    let owl = with_owl.path();
    let stdout = |out: Output| {
        assert_eq!(out.status.code(), Some(0), "{:?}", out.status);
        assert_eq!(String::from_utf8_lossy(&out.stderr), "", "stderr");
        String::from_utf8(out.stdout).unwrap()
    };

    // 0 unseen: nothing on the plain hooks, the arm line alone at session start.
    let empty = home_with(0, false);
    assert_silent(&run_hook(owl, empty.path()), "0 records, no flag");
    assert_eq!(
        stdout(run_hook_args(
            owl,
            empty.path(),
            &["--hook-event", "SessionStart", "--session-start"]
        )),
        format!("{CLAUDE_ARM_ZERO}\n")
    );
    // 2 unseen: counter alone on the plain hooks, counter + arm at session start.
    let two = home_with(2, false);
    assert_eq!(stdout(run_hook(owl, two.path())), format!("{CLAUDE_TWO}\n"));
    assert_eq!(
        stdout(run_hook_args(
            owl,
            two.path(),
            &["--hook-event", "SessionStart", "--session-start"]
        )),
        format!("{CLAUDE_ARM_TWO}\n")
    );
    let spool = Spool::new(two.path()).unwrap();
    assert_eq!(spool.list(Dir::Inbox, |r| !r.seen).unwrap().len(), 2);

    // `{"watch": false}`: session start keeps the previews, drops the arm sentence.
    for home in [&two, &empty] {
        std::fs::write(home.path().join("plugin.json"), r#"{"watch": false}"#).unwrap();
    }
    assert_eq!(
        stdout(run_hook_args(
            owl,
            two.path(),
            &["--hook-event", "SessionStart", "--session-start"]
        )),
        format!("{}\n", CLAUDE_ARM_TWO.replace(&format!(" {ARM}"), ""))
    );
    assert_silent(
        &run_hook_args(
            owl,
            empty.path(),
            &["--hook-event", "SessionStart", "--session-start"],
        ),
        "watch off, 0 records",
    );
    // `{"watch": true}` restores the arm sentence.
    std::fs::write(empty.path().join("plugin.json"), r#"{"watch": true}"#).unwrap();
    assert_eq!(
        stdout(run_hook_args(
            owl,
            empty.path(),
            &["--hook-event", "SessionStart", "--session-start"]
        )),
        format!("{CLAUDE_ARM_ZERO}\n")
    );
    // The script stays a no-op with the flag when owl is missing or fails.
    let without_owl = path_dir(false);
    assert_silent(
        &run_hook_args(
            without_owl.path(),
            empty.path(),
            &["--hook-event", "SessionStart", "--session-start"],
        ),
        "no owl on PATH",
    );
    let file = tempfile::tempdir().unwrap();
    let file = file.path().join("home");
    std::fs::write(&file, b"").unwrap();
    assert_silent(
        &run_hook_args(
            owl,
            &file,
            &["--hook-event", "SessionStart", "--session-start"],
        ),
        "owl failing",
    );
    // An uninitialised home (no key, no config) counts 0 without failing, so the arm line
    // still goes out: arming is gated on the stored choice only, not on `owl init`.
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        stdout(run_hook_args(
            owl,
            dir.path(),
            &["--hook-event", "SessionStart", "--session-start"]
        )),
        format!("{CLAUDE_ARM_ZERO}\n")
    );
}

#[test]
fn hook_script_is_silent_when_owl_fails() {
    let with_owl = path_dir(true);
    // A home that is a regular file makes `owl` exit 1 with an error on stderr.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("home");
    std::fs::write(&file, b"").unwrap();
    let direct = Command::new(OWL)
        .env("OWLPOST_HOME", &file)
        .args(["inbox", "--count", "--format", "claude"])
        .output()
        .unwrap();
    assert_eq!(direct.status.code(), Some(1), "fixture must make owl fail");
    assert!(!direct.stderr.is_empty(), "owl must complain on stderr");
    assert_silent(&run_hook(with_owl.path(), &file), "owl failing");
}

#[test]
fn hook_script_is_silent_on_uninitialised_home() {
    let with_owl = path_dir(true);
    let dir = tempfile::tempdir().unwrap();
    // An existing but empty home (no key, no config, no spool).
    assert_silent(&run_hook(with_owl.path(), dir.path()), "empty home");
    // A home directory that does not exist at all.
    assert_silent(
        &run_hook(with_owl.path(), &dir.path().join("missing")),
        "missing home",
    );
}

// ---------- AC1 ----------

#[test]
fn manifests_parse_and_register_hooks() {
    let plugin_json = json(".claude-plugin/plugin.json");
    assert_eq!(plugin_json["name"], "owlpost");
    assert!(
        plugin_json["description"]
            .as_str()
            .is_some_and(|d| !d.is_empty())
    );

    let hooks = json("hooks/hooks.json");
    let table = hooks["hooks"]
        .as_object()
        .expect("hooks.json has a `hooks` object");
    assert_eq!(table.len(), EVENTS.len(), "exactly the three events");
    for event in EVENTS {
        let matchers = table[event]
            .as_array()
            .unwrap_or_else(|| panic!("{event} missing"));
        assert_eq!(matchers.len(), 1, "{event}: one matcher group");
        let cmds = matchers[0]["hooks"].as_array().unwrap();
        assert_eq!(cmds.len(), 1, "{event}: one command hook");
        assert_eq!(cmds[0]["type"], "command", "{event}");
        // OWL-023 AC2: `--session-start` on SessionStart only.
        // The line's hookEventName must match the event or Claude Code rejects the hook output.
        let expected = match event {
            "SessionStart" => {
                "${CLAUDE_PLUGIN_ROOT}/hooks/owl-count.sh --hook-event SessionStart --session-start"
            }
            "PostToolUse" => "${CLAUDE_PLUGIN_ROOT}/hooks/owl-count.sh --hook-event PostToolUse",
            _ => "${CLAUDE_PLUGIN_ROOT}/hooks/owl-count.sh",
        };
        assert_eq!(cmds[0]["command"], expected, "{event}");
        assert_eq!(cmds[0]["timeout"], 5, "{event}: 5 s timeout");
    }
    // The script forwards its arguments to `owl inbox --count --format claude`.
    assert!(
        read("hooks/owl-count.sh").contains("owl inbox --count --format claude \"$@\""),
        "owl-count.sh must forward \"$@\""
    );

    let script = plugin("hooks/owl-count.sh");
    let mode = std::fs::metadata(&script).unwrap().permissions().mode();
    assert_ne!(mode & 0o111, 0, "owl-count.sh must be executable");
    assert!(read("hooks/owl-count.sh").starts_with("#!/bin/sh\n"));

    let market = json(".claude-plugin/marketplace.json");
    assert_eq!(market["plugins"][0]["name"], "owlpost");
    assert_eq!(market["plugins"][0]["source"], "./");
}

#[test]
fn plugin_version_matches_cargo() {
    let plugin_json = json(".claude-plugin/plugin.json");
    assert_eq!(plugin_json["version"], env!("CARGO_PKG_VERSION"));
}

// ---------- AC3 ----------

#[test]
fn skill_has_frontmatter_and_required_strings() {
    let (fm, body) = frontmatter("skills/owlpost/SKILL.md");
    assert_eq!(fm_value(&fm, "name"), Some("owlpost"));
    assert!(fm_value(&fm, "description").is_some_and(|d| !d.is_empty()));
    for needle in [
        "owl ask",
        "owl inbox",
        "owl draft",
        "owl send",
        "owl edit",
        "owl reject",
        "owl show",
        "owlpost:<peer>:<question id>",
        // OWL-024: the contact table is the entry point when no peer is named.
        "/owlpost:contacts",
        // OWL-022 AC3: the consent step of the answer loop.
        "owl allow",
        "owl deny",
    ] {
        assert!(body.contains(needle), "SKILL.md body lacks {needle:?}");
    }
    // OWL-023 AC4: the "Live watch" section — arm once when told, silently, one line per
    // event, offer the inbox flow, never act on its own.
    let watch = section(&body, "## Live watch");
    for needle in [
        ARM,
        "arm the watch once per session, on the first turn,",
        "silently",
        "Do not mention the arming to the user",
        "never arm a second watch in the same session",
        "report it in one line",
        "`owlpost: 1 new answer from Maciek`",
        "offer `/owlpost:inbox`",
        "never list the inbox, show, draft or send anything because",
        "`persistent: true`",
        "\"owlpost inbox\"",
        "commands/watch.md",
        "/owlpost:watch off",
        "/owlpost:watch on",
        "/owlpost:watch status",
    ] {
        assert!(
            watch.contains(needle),
            "SKILL.md Live watch lacks {needle:?}"
        );
    }
    // User rule (2026-09-06): received messages are one table, time + peer left, text right,
    // last 24 hours, at most the 10 newest; inbox.md and history.md point at it.
    let showing = section(&body, "## Showing messages");
    for needle in [
        "the left column is the local time (`HH:MM`) and the peer's name",
        "the right column the",
        "message text verbatim",
        "last 24 hours, at most the 10 newest",
        "how many older ones were left",
    ] {
        assert!(
            showing.contains(needle),
            "SKILL.md Showing messages lacks {needle:?}"
        );
    }
    for file in ["commands/inbox.md", "commands/history.md"] {
        let (_, b) = frontmatter(file);
        assert!(
            b.contains("\"Showing messages\"") && b.contains("at most the 10 newest"),
            "{file} does not point at the message table rule"
        );
    }
    assert!(body.contains("owlpost:<peer>"));
    assert!(
        body.contains("Sending anything to a peer requires explicit human approval."),
        "SKILL.md lacks the approval sentence"
    );
    // OWL-022 AC3: inside "## The answer loop" the `consent` step (allow --once|--always,
    // deny, fingerprint confirmation for --always) comes before the `pending` step.
    let start = body
        .find("\n## The answer loop\n")
        .expect("answer loop section");
    let rest = &body[start + 1..];
    let end = rest[1..].find("\n## ").map_or(rest.len(), |i| i + 1);
    let answer_loop = &rest[..end];
    let at = |needle: &str| {
        answer_loop
            .find(needle)
            .unwrap_or_else(|| panic!("answer loop lacks {needle:?}"))
    };
    let consent = at("A record in state `consent`");
    let pending = at("A `pending` record");
    assert!(
        consent < pending,
        "consent step must precede the pending step"
    );
    for needle in [
        "owl allow <peer> --once",
        "owl allow <peer> --always",
        "owl deny <peer>",
        "fingerprint out-of-band",
        "--i-verified-the-fingerprint",
    ] {
        let pos = at(needle);
        assert!(
            consent <= pos && pos < pending,
            "{needle:?} must sit in the consent step, before pending"
        );
    }
}

/// The body of the `heading` section of a Markdown text, up to the next `## ` heading.
fn section<'a>(body: &'a str, heading: &str) -> &'a str {
    let start = body
        .find(&format!("\n{heading}\n"))
        .unwrap_or_else(|| panic!("no {heading:?} section"));
    let rest = &body[start + 1..];
    let end = rest[1..].find("\n## ").map_or(rest.len(), |i| i + 1);
    &rest[..end]
}

// ---------- AC4 ----------

/// Sorted stems of every `commands/*.md` file: the directory listing drives every check
/// below, so a command added later is covered without touching this file.
fn command_names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(plugin("commands"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .map(|p| p.file_stem().unwrap().to_str().unwrap().to_string())
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no command files");
    names
}

/// Sorted subcommand names from the `Commands:` section of `owl [sub] --help`: clap prints
/// each as a two-space-indented word at the start of a line, up to the next blank line.
fn help_subcommands(args: &[&str]) -> Vec<String> {
    let out = Command::new(OWL).args(args).arg("--help").output().unwrap();
    assert!(
        out.status.success(),
        "owl {args:?} --help: {:?}",
        out.status
    );
    let text = String::from_utf8(out.stdout).unwrap();
    let rest = text
        .split_once("\nCommands:\n")
        .unwrap_or_else(|| panic!("owl {args:?} --help has no Commands section"))
        .1;
    let mut names: Vec<String> = rest
        .lines()
        .take_while(|l| !l.trim().is_empty())
        .filter(|l| l.starts_with("  ") && !l.starts_with("   "))
        .map(|l| l.split_whitespace().next().unwrap().to_string())
        .filter(|n| n != "help")
        .collect();
    names.sort();
    names
}

/// `owl <sub> --help` output.
fn help(sub: &str) -> String {
    let out = Command::new(OWL).arg(sub).arg("--help").output().unwrap();
    assert!(out.status.success(), "owl {sub} --help: {:?}", out.status);
    String::from_utf8(out.stdout).unwrap()
}

/// Whether `owl <sub>` takes anything beyond the global options: a positional (`Arguments:`
/// section) or a nested subcommand (`<COMMAND>` in the usage line). Every subcommand
/// accepts `[OPTIONS]`, so that alone does not count.
fn takes_arguments(sub: &str) -> bool {
    let h = help(sub);
    h.contains("\nArguments:\n") || h.contains("<COMMAND>")
}

/// The `owl` subcommand a command file wraps: the file stem, except the two files whose
/// name is not a subcommand (`me` = `owl contact export`, `contacts` = `owl contact list`).
fn wrapped_subcommand(name: &str) -> &str {
    match name {
        "me" => "contact export",
        "contacts" => "contact",
        other => other,
    }
}

/// OWL-021 AC1: every `owl --help` subcommand except `daemon` (a service; install/uninstall/
/// doctor cover it) and `mcp` (OWL-024: a stdio server Claude Code starts itself) has
/// `commands/<name>.md`; `contact` has both `contacts.md` (the OWL-024 table over
/// `contact list`) and `contact.md` (`show|export|remove`). The reverse holds too: every
/// command file wraps a real subcommand, `me` and `contacts` being the two renamed ones.
#[test]
fn every_subcommand_has_a_command() {
    let subs = help_subcommands(&[]);
    assert!(subs.contains(&"daemon".to_string()), "{subs:?}");
    assert!(subs.contains(&"mcp".to_string()), "{subs:?}");
    assert!(subs.contains(&"contact".to_string()), "{subs:?}");
    let names = command_names();
    for sub in &subs {
        if sub == "daemon" || sub == "mcp" {
            assert!(!names.contains(sub), "{sub} must not get a command file");
            continue;
        }
        assert!(names.contains(sub), "commands/{sub}.md is missing");
    }
    assert!(
        names.contains(&"contacts".to_string()),
        "commands/contacts.md"
    );
    // `contact.md` covers the non-list subcommands by name.
    let (_, contact) = frontmatter("commands/contact.md");
    for sub in help_subcommands(&["contact"]) {
        if sub == "list" {
            assert!(contact.contains("/owlpost:contacts"), "{contact}");
        } else {
            assert!(
                contact.contains(&format!("`{sub}")),
                "contact.md lacks {sub}: {contact}"
            );
        }
    }
    for name in &names {
        let sub = wrapped_subcommand(name).split(' ').next().unwrap();
        assert!(
            subs.contains(&sub.to_string()),
            "commands/{name}.md wraps no subcommand"
        );
    }
}

/// OWL-021 AC2. The `allowed-tools` rule, encoded here and nowhere else:
/// 1. the primary pattern `Bash(owl <wrapped sub>:*)` is present (`me.md` →
///    `Bash(owl contact export:*)`, `contacts.md` → `Bash(owl contact:*)`);
/// 2. every other entry is `Bash(owl <X>:*)` where `<X>` starts with a real `owl`
///    subcommand and the body actually runs or offers `owl <X>` (so a helper pattern
///    cannot outlive its use; OWL-022 will give `inbox.md` several such entries);
/// 3. the only non-`owl` entry ever allowed is `Bash(git blame:*)`, and only in `ask.md`
///    and `contacts.md` (the ask flow's peer proposal); nothing else, no `Read`, no
///    `Bash(git status:*)`.
/// 4. the one exemption: `watch.md` (OWL-023) does not wrap `owl watch` at all — it is the
///    Monitor-based live watch, so its set is pinned exactly to [`WATCH_TOOLS`] here and
///    checked in detail by `watch_command_is_a_monitor_toggle`.
#[test]
fn commands_have_descriptions() {
    let subs = help_subcommands(&[]);
    for name in command_names() {
        let rel = format!("commands/{name}.md");
        let (fm, body) = frontmatter(&rel);
        assert!(
            fm_value(&fm, "description").is_some_and(|d| !d.is_empty()),
            "{rel}: empty description"
        );
        assert!(!body.trim().is_empty(), "{rel}: empty body");

        let wrapped = wrapped_subcommand(&name);
        let tools =
            fm_value(&fm, "allowed-tools").unwrap_or_else(|| panic!("{rel}: no allowed-tools"));
        if name == "watch" {
            let mut have: Vec<&str> = tools.split(',').map(str::trim).collect();
            have.sort_unstable();
            let mut want = WATCH_TOOLS;
            want.sort_unstable();
            assert_eq!(have, want, "{rel}: allowed-tools");
            continue;
        }
        let primary = format!("Bash(owl {wrapped}:*)");
        let entries: Vec<&str> = tools.split(',').map(str::trim).collect();
        assert!(
            entries.contains(&primary.as_str()),
            "{rel}: allowed-tools lacks {primary}: {tools}"
        );
        for entry in entries {
            if entry == primary {
                continue;
            }
            if entry == "Bash(git blame:*)" && (name == "ask" || name == "contacts") {
                continue;
            }
            let x = entry
                .strip_prefix("Bash(owl ")
                .and_then(|r| r.strip_suffix(":*)"))
                .unwrap_or_else(|| {
                    panic!("{rel}: allowed-tools entry {entry:?} is not an owl pattern")
                });
            let first = x.split(' ').next().unwrap();
            assert!(
                subs.contains(&first.to_string()),
                "{rel}: {entry} is not an owl subcommand"
            );
            assert!(
                body.contains(&format!("owl {x}")),
                "{rel}: {entry} allowed but `owl {x}` never used in the body"
            );
        }

        // A wrapper of a subcommand with positionals must pass `$ARGUMENTS` through — except
        // the arg-less contact table, which the OWL-024 test pins to take none.
        let sub = wrapped.split(' ').next().unwrap();
        if takes_arguments(sub) && name != "contacts" && name != "me" {
            assert!(
                body.contains("$ARGUMENTS"),
                "{rel}: `owl {sub}` takes arguments but the body never mentions $ARGUMENTS"
            );
            assert!(
                fm_value(&fm, "argument-hint").is_some_and(|h| !h.trim_matches('"').is_empty()),
                "{rel}: `owl {sub}` takes arguments but there is no argument-hint"
            );
        }
    }
    // The set of arg-taking subcommands the rule above derives from `--help`, pinned so a
    // clap change that drops the `Arguments:` section is noticed.
    let with_args: Vec<&str> = [
        "card", "contact", "add", "allow", "deny", "ask", "show", "draft", "edit", "send", "reject",
    ]
    .to_vec();
    for sub in &subs {
        if sub == "daemon" {
            continue;
        }
        assert_eq!(
            takes_arguments(sub),
            with_args.contains(&sub.as_str()),
            "owl {sub} --help argument detection"
        );
    }

    let (fm, ask) = frontmatter("commands/ask.md");
    assert!(ask.contains("$ARGUMENTS"), "ask.md must read $ARGUMENTS");
    // OWL-018 AC4: the path is optional; both invocations are documented.
    assert_eq!(
        fm_value(&fm, "argument-hint"),
        Some("\"<peer> [path] <question...>\"")
    );
    assert!(ask.contains("`<peer> [path] <question...>`"), "{ask}");
    assert!(
        ask.contains("`owl ask <peer> \"<question>\"`"),
        "the no-path invocation: {ask}"
    );
    assert!(
        ask.contains("`owl ask --file <path> --peer <peer> \"<question>\"`"),
        "the with-path invocation: {ask}"
    );
    assert!(
        !ask.contains("If any part is missing, ask the user"),
        "must not demand a path: {ask}"
    );
    // OWL-024 AC6: the command is the approval; no confirmation step.
    assert!(
        ask.contains("Do not ask for confirmation: the command is the approval."),
        "{ask}"
    );
    assert!(
        !ask.contains("Ask for explicit approval"),
        "ask.md must not confirm: {ask}"
    );
    assert!(!ask.contains("AskUserQuestion"), "{ask}");
}

/// OWL-021 AC3: the commands that send something or change trust confirm once, with the
/// one literal sentence every such file carries, before running `owl`.
#[test]
fn trust_changing_commands_confirm_before_running() {
    const CONFIRM: &str = "Ask for explicit confirmation with AskUserQuestion before running";
    for name in ["send", "allow", "deny", "reject", "uninstall"] {
        let rel = format!("commands/{name}.md");
        let (_, body) = frontmatter(&rel);
        let at = body
            .find(CONFIRM)
            .unwrap_or_else(|| panic!("{rel}: lacks {CONFIRM:?}"));
        // The confirmation comes before the run step.
        let run = body
            .find(&format!("Run `owl {name}"))
            .unwrap_or_else(|| panic!("{rel}: never runs owl {name}"));
        assert!(at < run, "{rel}: confirmation must precede the run step");
    }
    // `edit` cannot run its editor inside a session and says so instead of running it.
    let (_, edit) = frontmatter("commands/edit.md");
    assert!(edit.contains("$EDITOR"), "{edit}");
    assert!(edit.contains("Do not run it here"), "{edit}");
}

// ---------- OWL-023: /owlpost:watch ----------

/// AC3: exactly the tools the live watch needs, nothing that lists, shows, drafts or sends.
const WATCH_TOOLS: [&str; 5] = ["Bash(owl inbox:*)", "Monitor", "TaskStop", "Read", "Write"];
/// The poll script, one line, verbatim.
const WATCH_SCRIPT: &str = r#"prev=""; while true; do cur=$(owl inbox --count --format plain 2>/dev/null || true); [ "$cur" != "$prev" ] && [ -n "$cur" ] && echo "$cur"; prev="$cur"; sleep 5; done"#;

#[test]
fn watch_command_is_a_monitor_toggle() {
    let (fm, body) = frontmatter("commands/watch.md");
    assert_eq!(fm_value(&fm, "argument-hint"), Some("\"on|off|status\""));
    assert_eq!(
        fm_value(&fm, "allowed-tools"),
        Some(WATCH_TOOLS.join(", ").as_str())
    );
    assert!(body.contains("$ARGUMENTS"), "watch.md must read $ARGUMENTS");
    for needle in [
        "## `on`",
        "## `off`",
        "## `status` (or no argument)",
        "`Monitor`",
        "`TaskStop`",
        "`plugin.json`",
        "`{\"watch\": false}`",
        "`{\"watch\": true}`",
        "${OWLPOST_HOME:-$HOME/.config/owlpost}",
        "`--format plain`",
        "`persistent: true`",
        "\"owlpost inbox\"",
        "The script emits only on change:",
        "`--format plain` prints nothing at zero",
        ARM,
        "never runs `owl inbox` without `--count` (listing marks records",
        "it never runs owl show, owl draft or owl send",
        "offer `/owlpost:inbox`",
        "Arm at most one watch per session",
        "belongs in a terminal",
    ] {
        assert!(body.contains(needle), "watch.md body lacks {needle:?}");
    }
    assert!(
        body.contains(WATCH_SCRIPT),
        "watch.md lacks the poll script"
    );
    // The three verbs appear only in that one "never" sentence, never as an instruction.
    for verb in ["owl show", "owl draft", "owl send"] {
        assert_eq!(
            body.matches(verb).count(),
            1,
            "watch.md mentions {verb:?} outside the never sentence"
        );
    }
    assert!(
        !body.contains("Run `owl watch"),
        "watch.md must not wrap owl watch"
    );
    // The stopping side comes with the stored choice: TaskStop before Write in `off`.
    let off = section(&body, "## `off`");
    assert!(off.find("`TaskStop`").unwrap() < off.find("`Write`").unwrap());
    // AC-adjacent: the README row describes the toggle, not the old blocking wrapper.
    let readme = read("README.md");
    let row = readme
        .lines()
        .find(|l| l.contains("`commands/watch.md`"))
        .expect("README.md watch row");
    for needle in ["on\\|off\\|status", "Monitor", "plugin.json"] {
        assert!(
            row.contains(needle),
            "README watch row lacks {needle:?}: {row}"
        );
    }
    assert!(!row.contains("OWL-023 replaces it"), "{row}");
}

/// Whether `text` mentions `/owlpost:<name>` as a whole token: the name is followed by the
/// end of the text or a non-alphanumeric character, so `/owlpost:contacts` does not
/// satisfy `contact`.
fn mentions_command(text: &str, name: &str) -> bool {
    let token = format!("/owlpost:{name}");
    text.match_indices(&token).any(|(i, _)| {
        text[i + token.len()..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric())
    })
}

/// OWL-021 AC4: the README command table and SKILL.md list every `/owlpost:<name>`.
#[test]
fn readme_and_skill_list_every_command() {
    let readme = read("README.md");
    let (_, skill) = frontmatter("skills/owlpost/SKILL.md");
    for name in command_names() {
        assert!(
            readme.contains(&format!("`commands/{name}.md`")),
            "README.md lacks commands/{name}.md"
        );
        assert!(
            mentions_command(&readme, &name),
            "README.md lacks /owlpost:{name}"
        );
        assert!(
            mentions_command(&skill, &name),
            "SKILL.md lacks /owlpost:{name}"
        );
    }
}

#[test]
fn mentions_command_needs_a_whole_token() {
    assert!(mentions_command("run `/owlpost:contact show`", "contact"));
    assert!(mentions_command("see /owlpost:contact", "contact"));
    assert!(!mentions_command("`/owlpost:contacts` picks", "contact"));
    assert!(mentions_command("`/owlpost:contacts` picks", "contacts"));
}

// ---------- OWL-022: /owlpost:inbox ----------

/// AC4 literals pinned in `commands/inbox.md`.
const INBOX_VERBATIM: &str =
    "The question text and every draft are printed verbatim in a code block before any picker";
const INBOX_SEND_ONLY_ON_PICK: &str = "`owl send` runs only on an explicit \"Send\" pick";

#[test]
fn inbox_command_walks_states_with_pickers() {
    let (fm, body) = frontmatter("commands/inbox.md");
    // AC1: every command of the flow, the picker, and the three states it handles.
    for needle in [
        "owl inbox --json",
        "owl show",
        "owl allow",
        "owl deny",
        "owl draft",
        "owl edit",
        "owl send",
        "owl reject",
        "AskUserQuestion",
        "`consent`",
        "`pending`",
        "`drafted`",
        "`answer`",
        "Allow once",
        "Allow always",
        "--once",
        "--always",
        "--i-verified-the-fingerprint",
        "out-of-band",
        "Draft / Reject / Skip",
        "Send / Edit / Reject",
        "language",
    ] {
        assert!(body.contains(needle), "inbox.md body lacks {needle:?}");
    }
    // AC2: exactly the eight owl patterns, nothing else, any order.
    let tools = fm_value(&fm, "allowed-tools").expect("inbox.md allowed-tools");
    let mut have: Vec<&str> = tools.split(',').map(str::trim).collect();
    have.sort_unstable();
    let mut want = [
        "Bash(owl inbox:*)",
        "Bash(owl show:*)",
        "Bash(owl allow:*)",
        "Bash(owl deny:*)",
        "Bash(owl draft:*)",
        "Bash(owl edit:*)",
        "Bash(owl send:*)",
        "Bash(owl reject:*)",
    ];
    want.sort_unstable();
    assert_eq!(have, want, "inbox.md allowed-tools");
    // AC4: the two ground-rule sentences, and the empty-inbox one-liner.
    assert!(
        body.contains(INBOX_VERBATIM),
        "inbox.md lacks the verbatim rule"
    );
    assert!(
        body.contains(INBOX_SEND_ONLY_ON_PICK),
        "inbox.md lacks the send-only-on-pick rule"
    );
    assert!(
        body.contains(r#"say "owlpost inbox is empty" and stop"#),
        "inbox.md lacks the empty-inbox one-liner"
    );
    // The consent step comes before pending, pending before drafted.
    let at = |needle: &str| body.find(needle).unwrap_or_else(|| panic!("no {needle:?}"));
    assert!(at("## 2. `consent`") < at("## 3. `pending`"));
    assert!(at("## 3. `pending`") < at("## 4. `drafted`"));
    // AC5: the README command table row describes the new flow.
    let readme = read("README.md");
    let row = readme
        .lines()
        .find(|l| l.contains("`commands/inbox.md`"))
        .expect("README.md inbox row");
    for needle in [
        "`/owlpost:inbox`",
        "owl inbox --json",
        "consent",
        "verbatim",
    ] {
        assert!(
            row.contains(needle),
            "README inbox row lacks {needle:?}: {row}"
        );
    }
}

// ---------- OWL-024: contacts as MCP resources ----------

/// The hint line `contacts.md` ends with, verbatim.
const MENTION_HINT: &str =
    "Type @owl: and the start of the name, pick the contact, then type the question.";
const MCP_ADD: &str = "claude mcp add --scope user owl -- owl mcp";

fn design_doc() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/docs/technical-design.md"
    ))
    .unwrap()
}

/// OWL-025 AC1: the plugin bundles no MCP server (a bundled one is named
/// `plugin:owlpost:owl`, which sinks the contacts in the `@` typeahead); the server is
/// registered at user scope by `owl setup` / `owl update`, and no doc says `.mcp.json`.
#[test]
fn plugin_bundles_no_mcp_json() {
    assert!(
        !plugin(".mcp.json").exists(),
        "plugins/claude-code/.mcp.json must not exist"
    );
    for (name, text) in [
        ("README.md", read("README.md")),
        ("SKILL.md", read("skills/owlpost/SKILL.md")),
        ("technical-design.md", design_doc()),
    ] {
        assert!(!text.contains(".mcp.json"), "{name} still says .mcp.json");
    }
    // An inline `mcpServers` in the manifest would bundle the server just the same.
    let manifest = json(".claude-plugin/plugin.json");
    let obj = manifest.as_object().expect("plugin.json object");
    assert!(!obj.contains_key("mcpServers"), "{manifest}");
    assert!(
        obj.get("experimental")
            .and_then(Value::as_object)
            .is_none_or(|e| !e.contains_key("mcpServers")),
        "{manifest}"
    );
}

/// AC6: `contacts.md` is a table over `owl contact list --json` ending with the mention
/// hint; no picker, no `owl ask`, only the `contact` pattern allowed.
#[test]
fn contacts_command_lists_and_points_at_mentions() {
    let (fm, body) = frontmatter("commands/contacts.md");
    assert!(
        fm_value(&fm, "argument-hint").is_none_or(|h| h.trim_matches('"').trim().is_empty()),
        "contacts.md must not take an argument: {fm}"
    );
    assert!(
        !body.contains("$ARGUMENTS"),
        "contacts.md must not read $ARGUMENTS"
    );
    assert_eq!(
        fm_value(&fm, "allowed-tools"),
        Some("Bash(owl contact:*)"),
        "{fm}"
    );
    for needle in [
        "owl contact list --json",
        "@owl:to://",
        "`@owl:to://<name-slug>.<email>`",
        MENTION_HINT,
        "No contacts yet — run /owlpost:add with a colleague's peer file.",
        "`-`",
    ] {
        assert!(body.contains(needle), "contacts.md body lacks {needle:?}");
    }
    assert_eq!(
        body.matches("owl contact ").count(),
        1,
        "contacts.md runs only `owl contact list --json`: {body}"
    );
    for forbidden in ["AskUserQuestion", "owl ask", "contact-pick", "picker"] {
        assert!(
            !body.contains(forbidden) && !fm.contains(forbidden),
            "contacts.md must not mention {forbidden:?}"
        );
    }
}

/// AC7: the skill's "Mentioned contact" section and the no-confirmation rule; the README
/// rows, the mention example and the `.mcp.json` note.
#[test]
fn skill_and_readme_document_mentions() {
    let (_, skill) = frontmatter("skills/owlpost/SKILL.md");
    let mentioned = section(&skill, "## Mentioned contact");
    for needle in [
        "@owl:to://",
        "owl ask --peer <fingerprint>",
        "the mention is the approval",
        "registered in Claude Code at user scope by `owl setup` / `owl update`",
        "whole repository",
        "Several mentions send the same question to each contact",
    ] {
        assert!(
            mentioned.contains(needle),
            "SKILL.md Mentioned contact lacks {needle:?}"
        );
    }
    // OWL-025 AC5: the `@owl:` typing hint, on one line.
    assert!(
        mentioned
            .lines()
            .any(|l| l.contains("type @owl: and the start of the name")),
        "SKILL.md Mentioned contact lacks the @owl: typing hint"
    );
    assert!(
        skill.contains("`/owlpost:ask` does not: the command itself is the approval."),
        "SKILL.md lacks the no-confirmation rule"
    );
    assert!(
        !skill.contains("as `/owlpost:ask`\ndoes."),
        "SKILL.md must not say /owlpost:ask confirms"
    );
    // The inbox keeps its `AskUserQuestion` pickers (OWL-022); the contact one is gone.
    assert!(
        !skill.contains("arrow-key"),
        "SKILL.md still describes the contact picker"
    );
    for line in skill.lines().filter(|l| l.contains("/owlpost:contacts")) {
        assert!(!line.contains("picker"), "{line}");
    }

    let readme = read("README.md");
    let row = |file: &str| {
        readme
            .lines()
            .find(|l| l.contains(&format!("`commands/{file}.md`")))
            .unwrap_or_else(|| panic!("README.md {file} row"))
            .to_string()
    };
    let contacts = row("contacts");
    assert!(contacts.contains("`/owlpost:contacts`"), "{contacts}");
    assert!(contacts.contains("@owl:to://"), "{contacts}");
    assert!(!contacts.contains("picker"), "{contacts}");
    let ask = row("ask");
    assert!(ask.contains("the command is the approval"), "{ask}");
    assert!(!ask.contains("confirms first"), "{ask}");
    let mention = section(&readme, "## Mention a contact");
    // OWL-025 AC5: the `@owl:krz` example, registration by `owl setup` / by hand, the
    // doctor check; each literal on one line.
    for needle in [
        "`@owl:to://ana-kowalska.ana@acme.pl",
        "`@owl:krz`",
        "`owl setup`",
        "`owl update`",
        MCP_ADD,
        "`owl doctor`",
        "`owl` on `PATH`",
        "mcp server owl already registered",
    ] {
        assert!(
            mention.lines().any(|l| l.contains(needle)),
            "README Mention a contact lacks {needle:?}"
        );
    }
    let row = readme
        .lines()
        .find(|l| l.starts_with("| MCP server |"))
        .expect("README MCP server row");
    assert!(row.contains("`owl mcp`"), "{row}");
    assert!(row.contains("`owl setup`"), "{row}");
}

/// AC8: the design doc lists `owl mcp` in the §9 CLI table and `cli/mcp.rs` in §2; OWL-025
/// AC5: the §9 `owl mcp` and `owl doctor` rows and §11 name the user-scope registration.
#[test]
fn design_doc_lists_owl_mcp() {
    let doc = design_doc();
    let layout = section(&doc, "## 2. Module layout");
    assert!(
        layout.contains("    mcp.rs           `owl mcp`"),
        "§2 lacks cli/mcp.rs"
    );
    let cli = section(&doc, "## 9. CLI contract");
    let row = cli
        .lines()
        .find(|l| l.starts_with("| `owl mcp` |"))
        .expect("§9 owl mcp row");
    for needle in [
        "stdio",
        "resources only",
        "to://<name-slug>.<first e-mail>",
        "re-read per request",
        MCP_ADD,
        "`mcp server owl already registered`",
        "checked by `owl doctor`",
    ] {
        assert!(
            row.contains(needle),
            "§9 owl mcp row lacks {needle:?}: {row}"
        );
    }
    let doctor = cli
        .lines()
        .find(|l| l.starts_with("| `owl doctor` |"))
        .expect("§9 owl doctor row");
    for needle in [
        "`ok mcp: owl registered in Claude Code (user scope)`",
        "`warn mcp: claude not on PATH`",
        MCP_ADD,
        "never `fail`",
    ] {
        assert!(
            doctor.contains(needle),
            "§9 owl doctor row lacks {needle:?}: {doctor}"
        );
    }
    let install = section(&doc, "## 11. Notifications and service install");
    for needle in [
        "`owl setup`",
        "`owl update`",
        "registration of the MCP server: `claude mcp add --scope user owl -- owl mcp`",
        "`mcp server owl already registered`",
        "`would run: claude mcp add --scope user owl -- owl mcp`",
        "`plugin:owlpost:owl`",
        "`mcp` check",
    ] {
        assert!(
            install.lines().any(|l| l.contains(needle)),
            "§11 lacks {needle:?}"
        );
    }
}

// ---------- AC5 helper ----------

/// Seeds `$OWLPOST_HOME` for the manual smoke run in `plugins/claude-code/README.md`:
/// `OWLPOST_HOME=$(mktemp -d) cargo test --test plugin seed_home_for_smoke -- --ignored --nocapture`.
#[test]
#[ignore = "manual smoke-run helper; writes into $OWLPOST_HOME"]
fn seed_home_for_smoke() {
    let home = std::env::var_os("OWLPOST_HOME").expect("export OWLPOST_HOME to a scratch dir");
    let home = PathBuf::from(home);
    seed(&home, &id(2), &id(1), 2, false);
    println!(
        "seeded {} with 2 unseen questions from Maciek",
        home.display()
    );
    println!("expected hook output:\n{CLAUDE_TWO}");
}
