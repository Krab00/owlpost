//! OWL-013 Claude Code plugin tests: the manifests parse and register the right hooks, the
//! hook script prints exactly the §9 injection line or nothing, and the skill and command
//! files carry the strings the plugin contract requires (AC1–AC4).

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use common::{Peer, id, policy, prepare_home, signed};
use owlpost::contacts::Mode;
use owlpost::envelope::{self, Body, Payload};
use owlpost::identity::Identity;
use owlpost::spool::{Dir, Record, Spool};
use serde_json::{Value, json};
use tempfile::TempDir;

const OWL: &str = env!("CARGO_BIN_EXE_owl");
const PLUGIN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/plugins/claude-code");
const CLAUDE_TWO: &str = r#"{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"🦉 owlpost: 2 new questions (Maciek 2). Say \"show owlpost inbox\" or run `owl inbox`."}}"#;
/// The three context-injecting events; `FileChanged` (the wake) and `SessionEnd` (the
/// session teardown, OWL-033) are the other two hook entries.
const EVENTS: [&str; 3] = ["SessionStart", "UserPromptSubmit", "PostToolUse"];
const ALL_EVENTS: [&str; 5] = [
    "SessionStart",
    "UserPromptSubmit",
    "PostToolUse",
    "FileChanged",
    "SessionEnd",
];
const SENTENCE_TWO: &str =
    "🦉 owlpost: 2 new questions (Maciek 2). Say \"show owlpost inbox\" or run `owl inbox`.";
/// The retired OWL-026/029 arm sentence prefix: no hook output may carry it any more (OWL-031).
const ARM_PREFIX: &str = "owlpost: before handling this prompt";
/// The two session-start previews `home_with(2, false)` produces, closed by the open sentence.
const PREVIEW_TWO: &str = "- Maciek question [pending] on src/auth/session.rs: why does session 0 retry?\n- Maciek question [pending] on src/auth/session.rs: why does session 1 retry?\nowlpost: run /owlpost:inbox now.";

/// The exact SessionStart hook line for `context`: serde's own escaping, §9 key order.
fn session_start_line(context: &str) -> String {
    format!(
        r#"{{"hookSpecificOutput":{{"hookEventName":"SessionStart","additionalContext":{}}}}}"#,
        serde_json::to_string(context).unwrap()
    )
}

/// The canonical `<home>/spool/inbox`: the one `watchPaths` entry `owl` emits (OWL-031 AC1).
/// The `watchPaths` entry (OWL-033): the canonical `<home>/sessions/<sid>/wake`, which
/// exists once the SessionStart hook ran.
fn wake_path(home: &Path, sid: &str) -> String {
    home.join("sessions")
        .join(sid)
        .join("wake")
        .canonicalize()
        .unwrap_or_else(|e| panic!("sessions/{sid}/wake must exist: {e}"))
        .to_string_lossy()
        .into_owned()
}

/// The SessionStart stdin JSON for `sid` (cwd `/c`, source `startup`).
fn start_stdin(sid: &str) -> String {
    format!(
        r#"{{"session_id":"{sid}","transcript_path":"/t","cwd":"/c","hook_event_name":"SessionStart","source":"startup"}}"#
    )
}

fn inbox_path(home: &Path) -> String {
    Spool::new(home).unwrap();
    home.join("spool")
        .join("inbox")
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// The SessionStart line with the watch on: `watchPaths` alone (0 unseen) and after the
/// two-question counter with its previews.
fn claude_watch_zero(home: &Path, sid: &str) -> String {
    format!(
        r#"{{"hookSpecificOutput":{{"hookEventName":"SessionStart","watchPaths":[{}]}}}}"#,
        serde_json::to_string(&wake_path(home, sid)).unwrap()
    )
}
fn claude_watch_two(home: &Path, sid: &str) -> String {
    format!(
        r#"{{"hookSpecificOutput":{{"hookEventName":"SessionStart","additionalContext":{},"watchPaths":[{}]}}}}"#,
        serde_json::to_string(&format!("{SENTENCE_TWO}\n{PREVIEW_TWO}")).unwrap(),
        serde_json::to_string(&wake_path(home, sid)).unwrap()
    )
}

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
            ..
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

/// [`run_hook`] with the script's arguments, the way `hooks.json` passes `--hook-event`;
/// stdin is closed (no hook input), so `owl` sees no session id.
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

/// [`run_hook_args`] with Claude Code's hook input JSON on stdin.
fn run_hook_stdin(path: &Path, home: &Path, args: &[&str], stdin: &str) -> Output {
    use std::io::Write;
    let mut child = Command::new("/bin/sh")
        .arg(plugin("hooks/owl-count.sh"))
        .args(args)
        .env_clear()
        .env("PATH", path)
        .env("OWLPOST_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
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

/// OWL-031 AC1 through the real script, per session since OWL-033: the SessionStart
/// invocation (`--hook-event SessionStart` forwarded by the script, the hook input JSON on
/// stdin) carries this session's `watchPaths` unless `plugin.json` switches the watch off;
/// with stdin closed (no session id) it carries none; the plain hooks never do; no output
/// carries the retired arm sentence.
#[test]
fn hook_script_forwards_session_start_and_reads_plugin_json() {
    let with_owl = path_dir(true);
    let owl = with_owl.path();
    let ss = ["--hook-event", "SessionStart"];
    let stdout = |out: Output| {
        assert_eq!(out.status.code(), Some(0), "{:?}", out.status);
        assert_eq!(String::from_utf8_lossy(&out.stderr), "", "stderr");
        let s = String::from_utf8(out.stdout).unwrap();
        assert!(!s.contains(ARM_PREFIX), "{s}");
        s
    };

    // 0 unseen: nothing on the plain hooks, the watchPaths line alone at session start.
    let empty = home_with(0, false);
    assert_silent(&run_hook(owl, empty.path()), "0 records, no flag");
    assert_eq!(
        stdout(run_hook_stdin(
            owl,
            empty.path(),
            &ss,
            &start_stdin("sess-1")
        )),
        format!("{}\n", claude_watch_zero(empty.path(), "sess-1"))
    );
    // Stdin closed: no session, so no watchPaths — and nothing at all at 0 unseen.
    assert_silent(
        &run_hook_args(owl, empty.path(), &ss),
        "SessionStart without hook input at 0",
    );
    // 2 unseen: counter alone on the plain hooks, counter + previews + watchPaths at start.
    let two = home_with(2, false);
    assert_eq!(stdout(run_hook(owl, two.path())), format!("{CLAUDE_TWO}\n"));
    assert_eq!(
        stdout(run_hook_args(
            owl,
            two.path(),
            &["--hook-event", "PostToolUse"]
        )),
        format!(
            "{}\n",
            CLAUDE_TWO.replace("UserPromptSubmit", "PostToolUse")
        )
    );
    assert_eq!(
        stdout(run_hook_stdin(owl, two.path(), &ss, &start_stdin("sess-2"))),
        format!("{}\n", claude_watch_two(two.path(), "sess-2"))
    );
    // Stdin closed with unseen records: the context without watchPaths.
    assert_eq!(
        stdout(run_hook_args(owl, two.path(), &ss)),
        format!(
            "{}\n",
            session_start_line(&format!("{SENTENCE_TWO}\n{PREVIEW_TWO}"))
        )
    );
    let spool = Spool::new(two.path()).unwrap();
    assert_eq!(spool.list(Dir::Inbox, |r| !r.seen).unwrap().len(), 2);

    // `{"watch": false}`: session start keeps the previews, drops watchPaths.
    for home in [&two, &empty] {
        std::fs::write(home.path().join("plugin.json"), r#"{"watch": false}"#).unwrap();
    }
    assert_eq!(
        stdout(run_hook_stdin(owl, two.path(), &ss, &start_stdin("sess-2"))),
        format!(
            "{}\n",
            session_start_line(&format!("{SENTENCE_TWO}\n{PREVIEW_TWO}"))
        )
    );
    assert_silent(
        &run_hook_stdin(owl, empty.path(), &ss, &start_stdin("sess-1")),
        "watch off, 0 records",
    );
    // `{"watch": true}` restores watchPaths.
    std::fs::write(empty.path().join("plugin.json"), r#"{"watch": true}"#).unwrap();
    assert_eq!(
        stdout(run_hook_stdin(
            owl,
            empty.path(),
            &ss,
            &start_stdin("sess-1")
        )),
        format!("{}\n", claude_watch_zero(empty.path(), "sess-1"))
    );
    // The script stays a no-op with the flag when owl is missing or fails.
    let without_owl = path_dir(false);
    assert_silent(
        &run_hook_stdin(
            without_owl.path(),
            empty.path(),
            &ss,
            &start_stdin("sess-1"),
        ),
        "no owl on PATH",
    );
    let file = tempfile::tempdir().unwrap();
    let file = file.path().join("home");
    std::fs::write(&file, b"").unwrap();
    assert_silent(
        &run_hook_stdin(owl, &file, &ss, &start_stdin("sess-1")),
        "owl failing",
    );
    // An uninitialised home (no key, no config) counts 0 without failing, so the watch path
    // still goes out: it is gated on the stored choice only, not on `owl init`.
    let dir = tempfile::tempdir().unwrap();
    let line = stdout(run_hook_stdin(owl, dir.path(), &ss, &start_stdin("sess-9")));
    assert_eq!(
        line,
        format!("{}\n", claude_watch_zero(dir.path(), "sess-9"))
    );
    let v: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(
        v["hookSpecificOutput"]["watchPaths"],
        json!([wake_path(dir.path(), "sess-9")])
    );
    assert!(
        v["hookSpecificOutput"]["watchPaths"][0]
            .as_str()
            .unwrap()
            .starts_with('/')
    );
}

/// Runs the real hook script through `env -i /bin/sh` with Claude Code's stdin JSON for
/// `event` (`extra` adds `FileChanged`'s `file_path`/`event` fields).
fn run_hook_env_i(owl: &Path, home: &Path, event: &str, extra: &str) -> Output {
    use std::io::Write;
    let mut child = Command::new("env")
        .arg("-i")
        .arg(format!("PATH={}", owl.display()))
        .arg(format!("OWLPOST_HOME={}", home.display()))
        .arg("/bin/sh")
        .arg(plugin("hooks/owl-count.sh"))
        .args(["--hook-event", event])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let input = format!(
        r#"{{"session_id":"sess-1","transcript_path":"/t","cwd":"/c","hook_event_name":"{event}"{extra}}}"#
    );
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

/// OWL-031 AC4 / OWL-033 AC9: the real hook script through `env -i /bin/sh` with Claude
/// Code's stdin JSON on all five events. `SessionStart` carries this session's `watchPaths`
/// (`<home>/sessions/sess-1/wake`); the other two context events never do; `FileChanged`
/// with an `add` of a file in that wake dir exits 2 with the file's content on stderr (the
/// pinned choice: the script lets owl's stderr through on that event only) and nothing on
/// stdout, and is exit 0 and silent for `change`, `unlink`, a path under `spool/inbox` (the
/// OWL-031 watch dir, whatever the count) and the watch off; `SessionEnd` exits 0 silently
/// and removes the session directory. Nothing is marked seen.
#[test]
fn hook_script_via_env_i_registers_the_watch_and_wakes_on_add() {
    let with_owl = path_dir(true);
    let owl = with_owl.path();
    let home = home_with(0, false);
    let inbox = inbox_path(home.path());
    let file_changed =
        |path: &str, event: &str| format!(r#","file_path":"{path}","event":"{event}""#);
    let context = |out: &Output| -> Option<Value> {
        assert_eq!(out.status.code(), Some(0), "{:?}", out.status);
        assert_eq!(String::from_utf8_lossy(&out.stderr), "");
        let line = String::from_utf8(out.stdout.clone()).unwrap();
        if line.is_empty() {
            return None;
        }
        assert_eq!(line.lines().count(), 1, "one line: {line:?}");
        assert!(!line.contains(ARM_PREFIX), "{line}");
        let v: Value = serde_json::from_str(line.trim()).unwrap_or_else(|e| panic!("{e}: {line}"));
        Some(v["hookSpecificOutput"].clone())
    };
    // 0 unseen: SessionStart registers this session's wake dir, the other two are silent.
    let ss = context(&run_hook_env_i(
        owl,
        home.path(),
        "SessionStart",
        r#","source":"startup""#,
    ))
    .unwrap();
    let wake = wake_path(home.path(), "sess-1");
    assert!(wake.ends_with("/sessions/sess-1/wake"), "{wake}");
    assert_eq!(
        ss,
        json!({"hookEventName": "SessionStart", "watchPaths": [wake]})
    );
    let marker: Value = serde_json::from_slice(
        &std::fs::read(home.path().join("sessions/sess-1/marker.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(marker["session_id"], "sess-1");
    assert_eq!(
        marker["cwd"], "/c",
        "a cwd that does not exist stays as given"
    );
    assert_eq!(marker["source"], "startup");
    for event in ["UserPromptSubmit", "PostToolUse"] {
        assert_eq!(
            context(&run_hook_env_i(owl, home.path(), event, "")),
            None,
            "{event}"
        );
    }
    // FileChanged on the OWL-031 inbox path: silent whatever the event and the count.
    seed(home.path(), &id(2), &id(1), 2, false);
    for event in ["add", "change", "unlink"] {
        assert_silent(
            &run_hook_env_i(
                owl,
                home.path(),
                "FileChanged",
                &file_changed(&format!("{inbox}/x.json"), event),
            ),
            &format!("FileChanged {event} under spool/inbox"),
        );
    }
    // A wake file lands in this session's dir (the daemon's `owl route`): `add` exits 2
    // with the file's content on stderr (the script re-adds the trailing newline it strips).
    let file = format!("{wake}/rec-1.md");
    let body = "| 🦉 **Maciek** · 10:00 · p · src/a.rs |\n|---|\n| why? |\n";
    std::fs::write(&file, body).unwrap();
    let woke = run_hook_env_i(owl, home.path(), "FileChanged", &file_changed(&file, "add"));
    assert_eq!(woke.status.code(), Some(2), "{:?}", woke.status);
    assert_eq!(
        String::from_utf8_lossy(&woke.stdout),
        "",
        "stdout stays empty"
    );
    assert_eq!(
        String::from_utf8(woke.stderr.clone()).unwrap(),
        body,
        "the wake file on stderr"
    );
    // `change` and `unlink` of the same file (the release) never wake.
    for event in ["change", "unlink"] {
        assert_silent(
            &run_hook_env_i(owl, home.path(), "FileChanged", &file_changed(&file, event)),
            &format!("FileChanged {event}"),
        );
    }
    // Another live session's wake dir, a look-alike dir and a missing file: silent.
    let now = envelope::rfc3339_now();
    std::fs::create_dir_all(home.path().join("sessions/sess-10/wake")).unwrap();
    std::fs::write(
        home.path().join("sessions/sess-10/marker.json"),
        json!({"session_id": "sess-10", "cwd": "/d", "started_at": now, "heartbeat_at": now, "source": "startup"}).to_string(),
    )
    .unwrap();
    std::fs::write(home.path().join("sessions/sess-10/wake/rec-1.md"), "other").unwrap();
    std::fs::create_dir_all(home.path().join("sessions/sess-1/wake-evil")).unwrap();
    std::fs::write(
        home.path().join("sessions/sess-1/wake-evil/rec-1.md"),
        "evil",
    )
    .unwrap();
    for other in [
        wake_path(home.path(), "sess-10") + "/rec-1.md",
        home.path()
            .join("sessions/sess-1/wake-evil/rec-1.md")
            .to_string_lossy()
            .into_owned(),
        format!("{wake}/missing.md"),
    ] {
        assert_silent(
            &run_hook_env_i(
                owl,
                home.path(),
                "FileChanged",
                &file_changed(&other, "add"),
            ),
            &format!("FileChanged add {other}"),
        );
    }
    // The context events carry the counter (SessionStart with previews and watchPaths).
    assert_eq!(
        context(&run_hook_env_i(owl, home.path(), "UserPromptSubmit", "")).unwrap(),
        json!({"hookEventName": "UserPromptSubmit", "additionalContext": SENTENCE_TWO})
    );
    assert_eq!(
        context(&run_hook_env_i(owl, home.path(), "SessionStart", "")).unwrap(),
        json!({"hookEventName": "SessionStart",
            "additionalContext": format!("{SENTENCE_TWO}\n{PREVIEW_TWO}"),
            "watchPaths": [wake]})
    );
    // Watch off: no wake, no watchPaths, previews kept.
    std::fs::write(home.path().join("plugin.json"), r#"{"watch": false}"#).unwrap();
    assert_silent(
        &run_hook_env_i(owl, home.path(), "FileChanged", &file_changed(&file, "add")),
        "FileChanged add, watch off",
    );
    assert_eq!(
        context(&run_hook_env_i(owl, home.path(), "SessionStart", "")).unwrap(),
        json!({"hookEventName": "SessionStart",
            "additionalContext": format!("{SENTENCE_TWO}\n{PREVIEW_TWO}")})
    );
    std::fs::remove_file(home.path().join("plugin.json")).unwrap();
    // SessionEnd: exit 0, silent, the session directory is gone; the other stays.
    assert_silent(
        &run_hook_env_i(owl, home.path(), "SessionEnd", r#","reason":"exit""#),
        "SessionEnd",
    );
    assert!(!home.path().join("sessions/sess-1").exists());
    assert!(home.path().join("sessions/sess-10").exists());
    // Nothing was marked seen by any of it; no `--follow` marker dir appeared.
    let spool = Spool::new(home.path()).unwrap();
    assert_eq!(spool.list(Dir::Inbox, |r| !r.seen).unwrap().len(), 2);
    assert!(!home.path().join("watch").exists());
    // A failing owl on FileChanged / SessionEnd is a silent exit 0 too (its stderr must not
    // leak as a wake).
    let broken = tempfile::tempdir().unwrap();
    let broken = broken.path().join("home");
    std::fs::write(&broken, b"").unwrap();
    assert_silent(
        &run_hook_env_i(owl, &broken, "FileChanged", &file_changed(&file, "add")),
        "owl failing on FileChanged",
    );
    assert_silent(
        &run_hook_env_i(owl, &broken, "SessionEnd", ""),
        "owl failing on SessionEnd",
    );
    let without_owl = path_dir(false);
    assert_silent(
        &run_hook_env_i(
            without_owl.path(),
            home.path(),
            "FileChanged",
            &file_changed(&file, "add"),
        ),
        "no owl on PATH on FileChanged",
    );
    assert_silent(
        &run_hook_env_i(without_owl.path(), home.path(), "SessionEnd", ""),
        "no owl on PATH on SessionEnd",
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
    assert_eq!(table.len(), ALL_EVENTS.len(), "exactly the five events");
    // OWL-031 AC4: the three context entries are byte-for-byte what OWL-029 left (no
    // `--session-start`, no asyncRewake), the FileChanged entry is the asyncRewake wake;
    // OWL-033 AC9: the SessionEnd entry tears the session's wake dir down (no asyncRewake).
    let entry = |command: &str, rewake: bool| {
        let mut hook = json!({"type": "command", "command": command, "timeout": 5});
        if rewake {
            hook["asyncRewake"] = json!(true);
        }
        json!([{"hooks": [hook]}])
    };
    assert_eq!(
        table["SessionStart"],
        entry(
            "${CLAUDE_PLUGIN_ROOT}/hooks/owl-count.sh --hook-event SessionStart",
            false
        )
    );
    assert_eq!(
        table["UserPromptSubmit"],
        entry("${CLAUDE_PLUGIN_ROOT}/hooks/owl-count.sh", false)
    );
    assert_eq!(
        table["PostToolUse"],
        entry(
            "${CLAUDE_PLUGIN_ROOT}/hooks/owl-count.sh --hook-event PostToolUse",
            false
        )
    );
    assert_eq!(
        table["FileChanged"],
        entry(
            "${CLAUDE_PLUGIN_ROOT}/hooks/owl-count.sh --hook-event FileChanged",
            true
        )
    );
    assert_eq!(
        table["SessionEnd"],
        entry(
            "${CLAUDE_PLUGIN_ROOT}/hooks/owl-count.sh --hook-event SessionEnd",
            false
        )
    );
    assert_eq!(table["SessionEnd"][0]["hooks"][0]["timeout"], 5);
    let wake = &table["FileChanged"][0]["hooks"][0];
    assert_eq!(wake["asyncRewake"], true, "FileChanged must asyncRewake");
    assert!(
        wake["command"]
            .as_str()
            .unwrap()
            .ends_with("owl-count.sh --hook-event FileChanged")
    );
    assert_eq!(wake["timeout"], 5);
    for event in EVENTS.iter().chain(["SessionEnd"].iter()) {
        assert!(
            table[*event][0]["hooks"][0].get("asyncRewake").is_none(),
            "{event} must not asyncRewake"
        );
    }
    assert!(
        read("hooks/hooks.json")
            .contains("${CLAUDE_PLUGIN_ROOT}/hooks/owl-count.sh --hook-event FileChanged"),
        "the placeholder stays literal in the file"
    );
    assert!(
        !read("hooks/hooks.json").contains("--session-start"),
        "hooks.json must not pass --session-start"
    );
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
    // OWL-031 AC5: the "Live watch" section — event-driven, nothing to arm, one line per
    // wake, offer the inbox flow, never act on its own; no arming vocabulary left.
    let watch = section(&body, "## Live watch");
    for needle in [
        "nothing to arm",
        "The live watch is event-driven and there is nothing to arm: the `SessionStart` hook",
        "registers this session's private wake directory as a watch path and the `FileChanged` hook",
        "exactly one session wakes per record, the others stay silent",
        "marked seen or moved away wakes nothing",
        "paste the\nmessage table it delivered verbatim",
        "offer `/owlpost:inbox`",
        "never list the inbox, draft or send anything because of a wake",
        "`/owlpost:watch off` stores `{\"watch\": false}`",
        "`/owlpost:watch on` restores it",
        "`/owlpost:watch status` reports the stored default (`commands/watch.md`)",
    ] {
        assert!(
            watch.contains(needle),
            "SKILL.md Live watch lacks {needle:?}"
        );
    }
    for gone in [
        ARM_PREFIX,
        "Monitor",
        "arm the inbox watch",
        "--follow",
        "persistent: true",
    ] {
        assert!(!body.contains(gone), "SKILL.md still says {gone:?}");
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
        "one fixed colour marker at the start of the left",
        "🟦 🟩 🟨 🟪 🟧 🟥",
        "the same peer keeps the same marker for the whole session",
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
/// doctor cover it), `mcp` (OWL-024: a stdio server Claude Code starts itself) and `route`
/// (OWL-033: the daemon's own routing call, for scripts and the e2e test) has
/// `commands/<name>.md`; `contact` has both `contacts.md` (the OWL-024 table over
/// `contact list`) and `contact.md` (`show|export|remove`). The reverse holds too: every
/// command file wraps a real subcommand, `me` and `contacts` being the two renamed ones.
#[test]
fn every_subcommand_has_a_command() {
    let subs = help_subcommands(&[]);
    assert!(subs.contains(&"daemon".to_string()), "{subs:?}");
    assert!(subs.contains(&"mcp".to_string()), "{subs:?}");
    assert!(subs.contains(&"route".to_string()), "{subs:?}");
    assert!(subs.contains(&"contact".to_string()), "{subs:?}");
    let names = command_names();
    for sub in &subs {
        if sub == "daemon" || sub == "mcp" || sub == "route" {
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
/// 4. the one exemption: `watch.md` (OWL-023/OWL-031) does not wrap `owl watch` at all — it
///    only reads and writes `plugin.json`, so its set is pinned exactly to [`WATCH_TOOLS`]
///    here and checked in detail by `watch_command_is_a_plugin_json_toggle`.
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
        "card", "contact", "add", "allow", "deny", "ask", "show", "draft", "edit", "send",
        "reject", "route", "status",
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
    // OWL-034 widened the parse line with the two optional flags; `[path]` stays optional.
    assert!(
        ask.contains("`<peer> [path] [--reply-to <id>] [--context <path>] <question...>`"),
        "{ask}"
    );
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

// ---------- OWL-023 / OWL-031: /owlpost:watch ----------

/// AC5: exactly the tools the toggle needs — it reads and writes `plugin.json`, nothing else;
/// no `Monitor`, no `TaskStop`, nothing that lists, shows, drafts or sends.
const WATCH_TOOLS: [&str; 2] = ["Read", "Write"];

#[test]
fn watch_command_is_a_plugin_json_toggle() {
    let text = read("commands/watch.md");
    let (fm, body) = frontmatter("commands/watch.md");
    assert_eq!(fm_value(&fm, "argument-hint"), Some("\"on|off|status\""));
    assert_eq!(
        fm_value(&fm, "allowed-tools"),
        Some(WATCH_TOOLS.join(", ").as_str())
    );
    for gone in ["Monitor", "TaskStop"] {
        assert!(
            !fm.contains(gone),
            "watch.md allowed-tools still has {gone}"
        );
    }
    assert!(body.contains("$ARGUMENTS"), "watch.md must read $ARGUMENTS");
    for needle in [
        "## `on`",
        "## `off`",
        "## `status` (or no argument)",
        "`plugin.json`",
        "`{\"watch\": false}`",
        "`{\"watch\": true}`",
        "${OWLPOST_HOME:-$HOME/.config/owlpost}",
        "The live watch is event-driven and needs nothing from you to run.",
        "registers this session's wake directory (`$OWLPOST_HOME/sessions/<session_id>/wake`) as a",
        "exactly one session wakes per record",
        "This command only reads and writes that file — it never runs `owl`.",
        "and nothing runs owl show, owl draft or owl send because of a wake",
        "offer `/owlpost:inbox`",
        "belongs in a terminal",
    ] {
        assert!(body.contains(needle), "watch.md body lacks {needle:?}");
    }
    // OWL-031 part C: the three exact status lines, one per section.
    let status = section(&body, "## `status` (or no argument)");
    assert!(
        status.contains(
            "`owlpost watch: default on|off; event-driven (FileChanged), nothing to arm`"
        ),
        "watch.md status lacks its line"
    );
    let off = section(&body, "## `off`");
    assert!(
        off.contains(
            "`owlpost watch: off; this session stops waking now, new sessions do not watch`"
        ),
        "watch.md off lacks its line"
    );
    assert!(off.contains("`{\"watch\": false}`"));
    let on = section(&body, "## `on`");
    assert!(
        on.contains("`owlpost watch: on; the inbox is watched from the next session start`"),
        "watch.md on lacks its line"
    );
    assert!(on.contains("`{\"watch\": true}`"));
    // Nothing of the Monitor era is left anywhere in the file.
    for gone in [
        "Monitor",
        "TaskStop",
        "marker",
        "arms on your next prompt",
        ARM_PREFIX,
        "--follow",
        "persistent: true",
        "owlpost watch: running",
        "prev=\"\"",
    ] {
        assert!(!text.contains(gone), "watch.md still says {gone:?}");
    }
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
    // The README row describes the event-driven toggle.
    let readme = read("README.md");
    let row = readme
        .lines()
        .find(|l| l.contains("`commands/watch.md`"))
        .expect("README.md watch row");
    for needle in [
        "on\\|off\\|status",
        "nothing to arm",
        "`FileChanged`",
        "plugin.json",
        "`status` reports the stored default",
    ] {
        assert!(
            row.contains(needle),
            "README watch row lacks {needle:?}: {row}"
        );
    }
    assert!(!row.contains("Monitor"), "{row}");
    assert!(!row.contains("OWL-023 replaces it"), "{row}");
    // The hooks row names the fourth hook.
    let hooks_row = readme
        .lines()
        .find(|l| l.starts_with("| Hooks |"))
        .expect("README.md hooks row");
    for needle in ["`watchPaths`", "`FileChanged`", "`asyncRewake`", "exit 2"] {
        assert!(
            hooks_row.contains(needle),
            "README hooks row lacks {needle:?}"
        );
    }
    // AC5: nothing under the plugin dir says Monitor (case-sensitive) or the retired phrases.
    for path in walk(Path::new(PLUGIN)) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for gone in [
            "Monitor",
            ARM_PREFIX,
            "arm the inbox watch",
            "it arms on your next prompt",
            "e2e-watch-arm",
        ] {
            assert!(
                !text.contains(gone),
                "{} still contains {gone:?}",
                path.display()
            );
        }
    }
}

/// Every regular file under `dir`, recursively.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
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
const INBOX_SEND_ONLY_ON_PICK: &str =
    "`owl send` runs only on an explicit \"Send\" or \"Draft & send\" pick";

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
        DRAFT_PICKS,
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

// ---------- OWL-028: one-pick "Draft & send" ----------

/// The step-3 picker of `commands/inbox.md`, verbatim.
const DRAFT_PICKS: &str = "Draft / Draft & send / Reject / Skip";
/// What the "Draft & send" option description must say.
const DRAFT_AND_SEND_DESC: &str = "sends the draft as-is; pick Draft to read it first";
/// The amended ground rule, one line, stated in SKILL.md, inbox.md and draft.md.
const NEVER_CHAIN: &str = "Never chain `owl draft` and `owl send` unless the human picked \"Draft & send\" (or passed `--send`); the draft is still printed in full before `owl send` runs.";
/// The two absolute forms the rule replaced.
const OLD_NEVER_CHAIN: [&str; 2] = [
    "Never chain draft and send in one step",
    "Never chain `owl draft` and `owl send` in one step",
];

#[test]
fn inbox_offers_draft_and_send_in_one_pick() {
    let (_, body) = frontmatter("commands/inbox.md");
    // AC1: the four picks, one per line, and the picker literal itself.
    let pending = section(
        &body,
        "## 3. `pending` records (question with no draft yet)",
    );
    assert!(
        pending.contains(DRAFT_PICKS),
        "inbox.md step 3 lacks {DRAFT_PICKS:?}"
    );
    for pick in [
        "- **Draft** —",
        "- **Draft & send** —",
        "- **Reject** —",
        "- **Skip** —",
    ] {
        assert!(
            pending.lines().any(|l| l.starts_with(pick)),
            "inbox.md step 3 lacks the pick line {pick:?}"
        );
    }
    let option = pending
        .lines()
        .skip_while(|l| !l.starts_with("- **Draft & send** —"))
        .take_while(|l| !l.starts_with("- **Reject** —"))
        .collect::<Vec<_>>()
        .join(" ");
    for needle in [
        DRAFT_AND_SEND_DESC,
        "run `owl draft <id>`",
        "print the draft verbatim in a code block",
        "then run `owl send <id>` at once, without a second picker",
        "non-zero `owl draft` exit",
        "nothing is sent",
        "non-zero `owl send` exit",
        "the record stays `drafted`",
    ] {
        assert!(
            option.contains(needle),
            "Draft & send option lacks {needle:?}: {option}"
        );
    }
    // Step 4's Send option is no longer the only pick that runs `owl send`.
    let drafted = section(&body, "## 4. `drafted` records (draft stored, not sent)");
    let send_option = drafted
        .lines()
        .skip_while(|l| !l.contains("- **Send** —"))
        .take_while(|l| !l.contains("- **Edit** —"))
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        send_option.contains("This is the only pick in this picker that runs `owl send`."),
        "inbox.md step 4 Send option lacks the picker-scoped sentence: {send_option}"
    );
    assert!(
        !body.contains("This is the only pick that runs `owl send`."),
        "inbox.md still has the unscoped only-pick sentence"
    );
    // The order of the draft-and-send steps: draft, print, send.
    let at = |needle: &str| {
        option
            .find(needle)
            .unwrap_or_else(|| panic!("no {needle:?}"))
    };
    assert!(at("run `owl draft <id>`") < at("print the draft verbatim"));
    assert!(at("print the draft verbatim") < at("then run `owl send <id>`"));
}

#[test]
fn draft_command_accepts_send_flag() {
    let (fm, body) = frontmatter("commands/draft.md");
    // AC2: `--send` documented, stripped before `owl draft`, draft printed before `owl send`.
    for needle in [
        "`/owlpost:draft <id> --send`",
        "`--send` is stripped from the",
        "arguments before `owl draft` runs (owl has no such flag)",
        "then `owl send <id>` runs at once",
        "non-zero `owl draft` exit",
        "nothing is sent",
        "non-zero `owl send` exit",
        "stays `drafted`",
    ] {
        assert!(body.contains(needle), "draft.md lacks {needle:?}");
    }
    assert!(
        fm_value(&fm, "argument-hint").is_some_and(|h| h.contains("[--send]")),
        "draft.md argument-hint lacks [--send]"
    );
    let tools = fm_value(&fm, "allowed-tools").expect("draft.md allowed-tools");
    let mut have: Vec<&str> = tools.split(',').map(str::trim).collect();
    have.sort_unstable();
    assert_eq!(
        have,
        ["Bash(owl draft:*)", "Bash(owl send:*)"],
        "draft.md allowed-tools"
    );
}

#[test]
fn never_chain_rule_is_amended_everywhere() {
    // AC3: the rule is stated three times, so it is pinned three times, one line each;
    // the old absolute forms are gone from every file that had them.
    for rel in [
        "skills/owlpost/SKILL.md",
        "commands/inbox.md",
        "commands/draft.md",
    ] {
        let (_, body) = frontmatter(rel);
        assert!(
            body.lines().any(|l| l == NEVER_CHAIN),
            "{rel}: lacks the amended never-chain line"
        );
        for old in OLD_NEVER_CHAIN {
            assert!(!body.contains(old), "{rel}: still has the old rule {old:?}");
        }
    }
    // The skill keeps its second sentence, and step 6 names the one-pick path.
    let (_, skill) = frontmatter("skills/owlpost/SKILL.md");
    let answer_loop = section(&skill, "## The answer loop");
    assert!(
        answer_loop.contains("Never send a draft the human has not seen in full."),
        "SKILL.md answer loop lacks the never-send-unseen sentence"
    );
    let step6 = answer_loop
        .lines()
        .skip_while(|l| !l.starts_with("6. "))
        .take_while(|l| l.starts_with("6. ") || l.starts_with("   "))
        .collect::<Vec<_>>()
        .join(" ");
    for needle in [
        "\"Draft & send\" pick",
        "`/owlpost:draft <id> --send`",
        "the draft is still printed in full before `owl send` runs",
    ] {
        assert!(
            step6.contains(needle),
            "SKILL.md step 6 lacks {needle:?}: {step6}"
        );
    }
    // The design doc never stated the rule, so nothing is pinned there; guard that.
    let design = design_doc();
    for old in OLD_NEVER_CHAIN {
        assert!(!design.contains(old), "design doc has the old rule {old:?}");
    }
    // The plugin README rows name the new pick and the flag.
    let readme = read("README.md");
    let row = |file: &str| {
        readme
            .lines()
            .find(|l| l.contains(file))
            .unwrap_or_else(|| panic!("README.md lacks the {file} row"))
            .to_string()
    };
    assert!(row("`commands/inbox.md`").contains("draft / draft & send / reject / skip picker"));
    assert!(row("`commands/draft.md`").contains("`--send` then runs `owl send` at once"));
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

// ---------- OWL-027/OWL-035: owl icon on the counter, message table for peer messages ----------

/// The counter prefix, verbatim (AC1).
const ICON: &str = "🦉 ";
/// The rule row of the one-column message table, verbatim (OWL-035 AC5).
const RULE_ROW: &str = "|---|";
/// The header row shape the skill documents (OWL-035 AC5).
const TABLE_HEADER: &str =
    "Header row: `| 🦉 **<peer>** · HH:MM · <project> · <path or \"whole repository\"> |`";
const DRAFTS_NOT_A_TABLE: &str = "Drafts (our own text) stay a plain code block and never become a table, so a table always means \"from a peer\".";
/// OWL-032: the CLI renders the block, the command says to paste it (was "print the framed
/// message block (see the skill, ...)", which pointed at text the model never had in context).
const SHOW_REF: &str = "run `owl show <id> --format claude` and paste its output verbatim";
const TABLE_REF: &str = "Run `owl inbox --format claude` and paste its output verbatim.";
const OLD_PHRASE: &str = "in a code block with peer name";
/// What a wake delivers since OWL-033: the message table, pasted verbatim (`commands/watch.md`).
const WATCH_EVENT: &str = "paste the message table it delivered verbatim";

/// `owl inbox --count --format <f>` against `home`, stdout as a string (exit 0).
fn count(home: &Path, format: &str) -> String {
    let out = Command::new(OWL)
        .args(["inbox", "--count", "--format", format])
        .env("OWLPOST_HOME", home)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{format}: {:?}", out.status);
    String::from_utf8(out.stdout).unwrap()
}

/// AC1: the counter sentence starts with `🦉 ` in plain and claude; zero prints nothing in
/// plain (and only `watchPaths` at session start); the `- <peer> ...` record lines and the
/// open sentence carry no icon.
#[test]
fn counter_wears_the_owl_icon_records_do_not() {
    let two = home_with(2, false);
    let plain = count(two.path(), "plain");
    assert_eq!(
        plain,
        format!(
            "{ICON}owlpost: 2 new questions (Maciek 2). Say \"show owlpost inbox\" or run `owl inbox`.\n"
        )
    );
    assert_eq!(count(two.path(), "claude"), format!("{CLAUDE_TWO}\n"));
    let context = |line: &str| -> String {
        let v: Value = serde_json::from_str(line.trim()).unwrap();
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(context(&count(two.path(), "claude")), plain.trim_end());
    // Zero: nothing in plain, and the watchPaths line without any context at session start.
    let empty = home_with(0, false);
    assert_eq!(count(empty.path(), "plain"), "");
    let with_owl = path_dir(true);
    let zero = String::from_utf8(
        run_hook_stdin(
            with_owl.path(),
            empty.path(),
            &["--hook-event", "SessionStart"],
            &start_stdin("sess-z"),
        )
        .stdout,
    )
    .unwrap();
    assert_eq!(
        zero,
        format!("{}\n", claude_watch_zero(empty.path(), "sess-z"))
    );
    assert!(!zero.contains('🦉'));
    // Two unseen at session start: icon once, on the counter; every preview line and the
    // open sentence are icon-free.
    let two_ss = String::from_utf8(
        run_hook_stdin(
            with_owl.path(),
            two.path(),
            &["--hook-event", "SessionStart"],
            &start_stdin("sess-t"),
        )
        .stdout,
    )
    .unwrap();
    let ctx = context(&two_ss);
    assert_eq!(
        ctx,
        context(&format!("{}\n", claude_watch_two(two.path(), "sess-t")))
    );
    assert_eq!(ctx.matches('🦉').count(), 1, "{ctx}");
    assert!(ctx.starts_with(ICON), "{ctx}");
    let lines: Vec<&str> = ctx.lines().collect();
    assert_eq!(lines.len(), 4, "{ctx}");
    assert_eq!(lines[0], SENTENCE_TWO);
    assert!(lines[1].starts_with("- Maciek question") && !lines[1].contains('🦉'));
    assert!(lines[2].starts_with("- Maciek question") && !lines[2].contains('🦉'));
    assert_eq!(lines[3], "owlpost: run /owlpost:inbox now.");
}

/// AC2 (OWL-035): SKILL.md "Showing messages" has the "Message table" subsection: the
/// example table, the header-row shape, the drafts-stay-a-code-block sentence and the
/// fingerprint-on-consent rule; nothing of the old orange frame is left.
#[test]
fn skill_documents_the_message_table() {
    let (_, body) = frontmatter("skills/owlpost/SKILL.md");
    let showing = section(&body, "## Showing messages");
    let table = showing
        .find("\n### Message table\n")
        .map(|i| &showing[i..])
        .expect("### Message table under Showing messages");
    assert!(
        !body.contains("### Framed message") && !body.contains("🟧🟧"),
        "the orange frame is gone from SKILL.md"
    );
    // The example is a real one-column table: header row, rule row, one body row.
    let example: Vec<&str> = table
        .lines()
        .skip_while(|l| !l.starts_with("| 🦉 "))
        .take(3)
        .collect();
    assert_eq!(
        example[0],
        "| 🦉 **Krzysztof Abramczyk** · 09:08 · github.com/Krab00/owlpost · whole repository |"
    );
    assert_eq!(example[1], RULE_ROW);
    assert_eq!(example[2], "| Jaki masz ostatni commit u Siebie? |");
    assert!(
        !table.contains("```text"),
        "the example never fences the message: {table}"
    );
    for needle in [
        TABLE_HEADER,
        "On a `consent` record the header also carries the peer's fingerprint",
        "one row per line of the message, verbatim, with `|` escaped as `\\|`",
        "an empty line is the row `|  |`",
        "The answers table above keeps the per-peer colour markers and stays two-column.",
    ] {
        assert!(table.contains(needle), "Message table lacks {needle:?}");
    }
    assert!(
        table.lines().any(|l| l == DRAFTS_NOT_A_TABLE),
        "Message table lacks the drafts-stay-a-code-block line"
    );
}

/// AC3: inbox.md steps 2 and 3 paste the rendered table, step 5 the rendered listing (the
/// old "in a code block with peer name" phrase is gone from them), step 4 keeps the plain
/// draft code block; watch.md says a wake delivers the message table (OWL-033/OWL-035),
/// never the icon-free counter event.
#[test]
fn inbox_steps_print_the_message_table_and_watch_event_has_the_icon() {
    let (_, body) = frontmatter("commands/inbox.md");
    let step = |n: usize| {
        let start = body
            .find(&format!("\n## {n}. "))
            .unwrap_or_else(|| panic!("inbox.md step {n}"));
        let rest = &body[start + 1..];
        let end = rest[1..].find("\n## ").map_or(rest.len(), |i| i + 1);
        &rest[..end]
    };
    for n in [2, 3, 5] {
        let s = step(n);
        let want = if n == 5 { TABLE_REF } else { SHOW_REF };
        assert!(
            s.to_lowercase().contains(&want.to_lowercase()),
            "inbox.md step {n} lacks {want:?}"
        );
        assert!(
            !s.contains(OLD_PHRASE),
            "inbox.md step {n} still says {OLD_PHRASE:?}"
        );
    }
    assert!(step(3).contains(SHOW_REF));
    assert!(step(2).contains("Run `owl show <id> --format claude` and paste its output verbatim"));
    assert!(
        step(2).contains("the fingerprint"),
        "consent header keeps the fingerprint"
    );
    let drafts = step(4);
    assert!(
        !drafts.contains("table"),
        "step 4 (drafts) must stay a plain code block: {drafts}"
    );
    assert!(
        drafts
            .contains("then `draft:` with the draft in a plain code block and `harness: <name>`.")
    );
    assert!(
        step(5).contains("stays two-column"),
        "the answers table keeps its own shape"
    );

    let (_, watch) = frontmatter("commands/watch.md");
    assert!(
        watch.contains(WATCH_EVENT),
        "watch.md lacks {WATCH_EVENT:?}"
    );
    assert!(
        !watch.contains("(\"owlpost: 1 new answer from"),
        "watch.md still quotes the icon-free event"
    );
}

// ---------- OWL-032/OWL-035: the CLI renders the message table ----------

/// AC6: end to end through the real binary — one consent question from a contact,
/// `owl show <id> --format claude`: the header row starts with `| 🦉 **`, row 2 is `|---|`
/// and the question is one plain row.
#[test]
fn show_format_claude_tables_a_consent_question_end_to_end() {
    let home = home_with(1, false);
    let spool = Spool::new(home.path()).unwrap();
    let (id, _) = spool.list(Dir::Inbox, |_| true).unwrap().remove(0);
    spool.set_state(Dir::Inbox, &id, "consent").unwrap();
    let out = Command::new(OWL)
        .args(["show", &id, "--format", "claude"])
        .env("OWLPOST_HOME", home.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{:?}", out.status);
    let stdout = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 3, "{stdout}");
    assert!(
        lines[0].starts_with("| 🦉 **Maciek** (owl:"),
        "{}",
        lines[0]
    );
    assert!(lines[0].ends_with(" |"), "{}", lines[0]);
    assert_eq!(lines[1], RULE_ROW, "{stdout}");
    assert_eq!(lines[2], "| why does session 0 retry? |", "{stdout}");
    assert!(
        !stdout.contains('🟧') && !stdout.contains("```"),
        "{stdout}"
    );
    assert!(
        spool.get(Dir::Inbox, &id).unwrap().unwrap().seen,
        "show marks seen"
    );
}

// ---------- OWL-034: threads, context and status ----------

/// AC8: `commands/ask.md` documents `--reply-to` and `--context`, `commands/status.md`
/// exists and wraps `owl status` (the parity tests above cover it by directory listing;
/// pinned here by name too), SKILL.md names `/owlpost:status`, `--reply-to` and
/// `--context`, and the plugin README has the status row.
#[test]
fn ask_documents_reply_to_and_context_and_status_wraps_owl_status() {
    let (fm, ask) = frontmatter("commands/ask.md");
    assert_eq!(
        fm_value(&fm, "argument-hint"),
        Some("\"<peer> [path] <question...>\"")
    );
    assert!(
        ask.contains("`--reply-to <id>` continues an earlier exchange with that peer"),
        "{ask}"
    );
    assert!(ask.contains("`--context <path>`"), "{ask}");
    assert!(
        ask.contains("attaches that file (a diff, an error, an excerpt; at most 8192 bytes)"),
        "{ask}"
    );
    assert!(
        ask.contains("append `--reply-to <id>` and `--context <path>` when given"),
        "{ask}"
    );
    assert!(ask.contains("`accepted <id> — <state>`"), "{ask}");
    let (fm, status) = frontmatter("commands/status.md");
    assert_eq!(fm_value(&fm, "allowed-tools"), Some("Bash(owl status:*)"));
    assert_eq!(fm_value(&fm, "argument-hint"), Some("\"[<id>]\""));
    assert!(status.contains("Run `owl status $ARGUMENTS`"), "{status}");
    assert!(
        status.contains("`ID  PEER  PATH  STATE  SINCE`"),
        "{status}"
    );
    // Single-line needles only (OWL-022): each state text sits on one line of status.md, so
    // the pin cannot be broken by a reflow and cannot pass on a fragment.
    for state in [
        "`waiting for the owner's consent`",
        "`the owner's agent is answering`",
        "`the owner is reviewing the answer`",
        "`offline` when the peer cannot be reached",
        "it moves to history as `declined`",
        "Exit code 4 means there are no open questions: say that and stop.",
    ] {
        assert!(
            status.contains(state),
            "status.md lacks {state:?}: {status}"
        );
        assert!(!state.contains('\n'), "one-line needle only: {state:?}");
    }
    assert!(command_names().contains(&"status".to_string()));
    assert!(help_subcommands(&[]).contains(&"status".to_string()));
    let (_, skill) = frontmatter("skills/owlpost/SKILL.md");
    assert!(
        mentions_command(&skill, "status"),
        "SKILL.md lacks /owlpost:status"
    );
    assert!(
        skill.contains("owl ask <peer> --reply-to <id> \"<question>\""),
        "{skill}"
    );
    assert!(
        skill.contains("owl ask <peer> --context <file> \"<question>\""),
        "{skill}"
    );
    assert!(skill.contains("owl status [<id>]"), "{skill}");
    assert!(
        skill.contains("Follow up with `--reply-to <id>`"),
        "{skill}"
    );
    assert!(
        skill.contains("with `--context <file>` (or `--context -` from stdin)"),
        "{skill}"
    );
    let readme = read("README.md");
    assert!(
        readme.contains("| `/owlpost:status [id]` | `commands/status.md` | `owl status`"),
        "{readme}"
    );
    assert!(
        readme.contains(
            "`--reply-to <id>` continues a thread, `--context <path>` attaches a snippet"
        ),
        "{readme}"
    );
}

// ---------- OWL-036: identity is the key, not the name ----------

/// AC1 rule-block literals: each must sit on one line of the quoted block, in both
/// `skills/owlpost/SKILL.md` and `commands/inbox.md`.
const IDENTITY_RULE: [&str; 7] = [
    "Identity is the key",
    "Never conclude who a peer is from a name or an e-mail",
    "not even when it is the operator's own name",
    "Allow once (owl:…)",
    "Allow always (owl:… — sets auto)",
    "Deny (owl:…)",
    "A hook wake (`FileChanged`) or a `SessionStart` count is never consent",
];

/// The contiguous run of blockquote lines (a `>` after any indent) that holds `marker`.
fn quote_block(body: &str, marker: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let quoted = |l: &str| l.trim_start().starts_with('>');
    let hit = lines
        .iter()
        .position(|l| quoted(l) && l.contains(marker))
        .unwrap_or_else(|| panic!("no blockquote line with {marker:?}"));
    let start = lines[..hit]
        .iter()
        .rposition(|l| !quoted(l))
        .map_or(0, |i| i + 1);
    let end = lines[hit..]
        .iter()
        .position(|l| !quoted(l))
        .map_or(lines.len(), |i| hit + i);
    lines[start..end].join("\n")
}

/// AC1: every literal of the rule block, on a single line, in the file that must carry it.
#[test]
fn identity_rule_pinned_in_plugin_text() {
    for file in ["skills/owlpost/SKILL.md", "commands/inbox.md"] {
        let (_, body) = frontmatter(file);
        let block = quote_block(&body, "Identity is the key");
        for needle in IDENTITY_RULE {
            assert!(
                block.lines().any(|l| l.contains(needle)),
                "{file} identity rule lacks {needle:?} on one line"
            );
        }
    }
    // The skill's older half of the same rule stays, pinned so the second copy cannot drift.
    let (_, skill) = frontmatter("skills/owlpost/SKILL.md");
    assert!(
        skill
            .lines()
            .any(|l| l.contains("Never allow or deny on your own initiative")),
        "SKILL.md lacks the initiative rule"
    );
    // The three picker labels where `commands/inbox.md` defines the picker itself.
    let (_, inbox) = frontmatter("commands/inbox.md");
    let start = inbox
        .find("`AskUserQuestion` with four options:")
        .expect("inbox.md consent picker");
    let picker = &inbox[start..];
    for label in [
        "**Allow once (owl:…)**",
        "**Allow always (owl:… — sets auto)**",
        "**Deny (owl:…)**",
    ] {
        assert!(
            picker.lines().any(|l| l.contains(label)),
            "inbox.md picker lacks {label:?}"
        );
    }
    // `commands/allow.md`: the fingerprint-over-name rule inside the `--always` step.
    let (_, allow) = frontmatter("commands/allow.md");
    let start = allow.find("1. Tell the user").expect("allow.md step 1");
    let end = allow
        .find("\n2. Ask for explicit")
        .expect("allow.md step 2");
    let step = &allow[start..end];
    assert!(step.contains("`--always`"), "allow.md step 1 lost --always");
    assert!(
        step.lines()
            .any(|l| l.contains("prefer the fingerprint, not a name")),
        "allow.md --always step lacks the fingerprint-over-name rule"
    );
}

/// AC2: the rule block sits inside the skill's consent step — after its opening sentence and
/// before the next numbered step — not in a section of its own.
#[test]
fn identity_rule_sits_in_the_skill_consent_step() {
    let (_, body) = frontmatter("skills/owlpost/SKILL.md");
    let answer_loop = section(&body, "## The answer loop");
    let at = |needle: &str| {
        answer_loop
            .find(needle)
            .unwrap_or_else(|| panic!("answer loop lacks {needle:?}"))
    };
    let consent = at("A record in state `consent`");
    let rule = at("**Identity is the key.**");
    let pending = at("A `pending` record");
    assert!(
        consent < rule,
        "the rule block must follow the consent step's opening sentence"
    );
    assert!(rule < pending, "the rule block must precede the next step");
}

/// OWL-037 AC5: `commands/ask.md` says the peer may arrive as an `@owl:to://` mention and is
/// passed on untouched (each literal on one line), and the skill's "Mentioned contact"
/// section carries the text-mention rule inside its own paragraph — after the attached-resource
/// sentence, before the bullet list that follows it.
#[test]
fn the_mention_is_a_peer_in_the_command_and_the_skill() {
    let (_, ask) = frontmatter("commands/ask.md");
    for needle in [
        "The peer may be an `@owl:to://` mention",
        "verbatim, as one shell-quoted word",
        "never rewritten into a name, e-mail or fingerprint",
    ] {
        assert!(
            ask.lines().any(|l| l.contains(needle)),
            "ask.md lacks {needle:?} on one line"
        );
    }

    let (_, skill) = frontmatter("skills/owlpost/SKILL.md");
    let mentioned = section(&skill, "## Mentioned contact");
    let at = |needle: &str| {
        mentioned
            .find(needle)
            .unwrap_or_else(|| panic!("Mentioned contact lacks {needle:?}"))
    };
    let attached = at("An attached `@owl:to://…` resource");
    let as_text = at("the text itself is the peer");
    let bullets = at("\n- The rest of the prompt is");
    assert!(
        attached < as_text,
        "the text-mention rule must follow the attached-resource sentence"
    );
    assert!(
        as_text < bullets,
        "the text-mention rule must sit before the bullet list"
    );
    assert!(
        mentioned
            .lines()
            .any(|l| l.contains("the text itself is the peer")),
        "the rule must be on one line"
    );
}
