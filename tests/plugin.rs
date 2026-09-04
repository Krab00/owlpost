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
            meta: json!({ "peer": p.from, "hash": envelope::question_hash(project, path, question) }),
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
    Command::new("/bin/sh")
        .arg(plugin("hooks/owl-count.sh"))
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
        assert_eq!(
            cmds[0]["command"], "${CLAUDE_PLUGIN_ROOT}/hooks/owl-count.sh",
            "{event}"
        );
        assert_eq!(cmds[0]["timeout"], 5, "{event}: 5 s timeout");
    }

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
    ] {
        assert!(body.contains(needle), "SKILL.md body lacks {needle:?}");
    }
    assert!(body.contains("owlpost:<peer>"));
    assert!(
        body.contains("Sending anything to a peer requires explicit human approval."),
        "SKILL.md lacks the approval sentence"
    );
}

// ---------- AC4 ----------

#[test]
fn commands_have_descriptions() {
    for name in ["inbox", "ask", "history", "me", "update", "add"] {
        let rel = format!("commands/{name}.md");
        let (fm, body) = frontmatter(&rel);
        assert!(
            fm_value(&fm, "description").is_some_and(|d| !d.is_empty()),
            "{rel}: empty description"
        );
        assert!(!body.trim().is_empty(), "{rel}: empty body");
    }
    let (_, ask) = frontmatter("commands/ask.md");
    assert!(ask.contains("$ARGUMENTS"), "ask.md must read $ARGUMENTS");
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
