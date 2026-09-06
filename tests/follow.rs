//! OWL-029 AC3/AC4: `owl inbox --count --follow --session <id>` (the live watch: marker with
//! its pid, counter only on change, nothing at zero, marker removed on a clean end) and the
//! hook path `owl inbox --count --format claude` reading `session_id` from stdin and arming
//! only while no live marker exists for that session. Every test drives the built binary
//! against its own temp home with a fast poll interval (`OWLPOST_FOLLOW_SECS`).

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Output, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use common::{Peer, id, policy, prepare_home, signed};
use owlpost::contacts::Mode;
use owlpost::envelope::{self, Body, Payload};
use owlpost::identity::Identity;
use owlpost::spool::{Dir, Record, Spool};
use serde_json::Value;
use tempfile::TempDir;

const OWL: &str = env!("CARGO_BIN_EXE_owl");
const EVENTS: [&str; 3] = ["SessionStart", "UserPromptSubmit", "PostToolUse"];
const ARM_PREFIX: &str = "owlpost: before handling this prompt";
/// The arm sentence for session `S`, exactly as `owl` emits it (AC4/AC5).
const ARM_S: &str = "owlpost: before handling this prompt, arm the inbox watch for this session: call Monitor with persistent: true, description \"owlpost inbox\" and this command (verbatim): `owl inbox --count --follow --session S`. Then handle the prompt.";
/// The arm sentence without a session id (no stdin: the old `SessionStart`-only rule).
const ARM_BARE: &str = "owlpost: before handling this prompt, arm the inbox watch for this session: call Monitor with persistent: true, description \"owlpost inbox\" and this command (verbatim): `owl inbox --count --follow`. Then handle the prompt.";
const STDIN_S: &str =
    r#"{"session_id":"S","transcript_path":"/t","cwd":"/c","hook_event_name":"UserPromptSubmit"}"#;

struct Home {
    dir: TempDir,
    me: Identity,
    maciek: Identity,
}

impl Home {
    fn new() -> Home {
        let dir = tempfile::tempdir().unwrap();
        let (me, maciek) = (id(2), id(1));
        prepare_home(
            dir.path(),
            &me,
            false,
            &[Peer::new(
                &maciek,
                "Maciek",
                Some(policy(Mode::Manual, None)),
            )],
        );
        Home { dir, me, maciek }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// One unseen pending question from Maciek.
    fn put(&self, text: &str) {
        let env = signed(&self.maciek, &self.me, text);
        let p: Payload = serde_json::from_str(&env.raw).unwrap();
        let Body::Question {
            project,
            path,
            question,
        } = &p.body
        else {
            unreachable!()
        };
        let rec = Record {
            raw: env.raw.clone(),
            sig: env.sig.clone(),
            state: "pending".into(),
            seen: false,
            received_at: envelope::rfc3339_now(),
            draft: None,
            meta: serde_json::json!({ "peer": p.from, "hash": envelope::question_hash(project, path.as_deref(), question) }),
        };
        Spool::new(self.path())
            .unwrap()
            .put(Dir::Inbox, &p.id, &rec)
            .unwrap();
    }

    fn mark_all_seen(&self) {
        let spool = Spool::new(self.path()).unwrap();
        for (id, _) in spool.list(Dir::Inbox, |r| !r.seen).unwrap() {
            spool.mark_seen(Dir::Inbox, &id).unwrap();
        }
    }

    fn unseen(&self) -> usize {
        Spool::new(self.path())
            .unwrap()
            .list(Dir::Inbox, |r| !r.seen)
            .unwrap()
            .len()
    }

    fn marker(&self, id: &str) -> std::path::PathBuf {
        self.path().join("watch").join(id)
    }

    fn owl(&self) -> Command {
        let mut c = Command::new(OWL);
        c.env("OWLPOST_HOME", self.path())
            .env("OWLPOST_FOLLOW_SECS", "0.05");
        c
    }

    /// `owl inbox --count --format claude --hook-event <event>` with `stdin` piped in
    /// (`None`: stdin closed, the way a harness without hook input runs it).
    fn hook(&self, event: &str, stdin: Option<&str>) -> Output {
        let mut cmd = self.owl();
        cmd.args([
            "inbox",
            "--count",
            "--format",
            "claude",
            "--hook-event",
            event,
        ])
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        if let Some(text) = stdin {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(text.as_bytes())
                .unwrap();
        }
        child.wait_with_output().unwrap()
    }
}

/// Every wait on a `--follow` child is bounded by this: a broken exit path must fail the
/// test, never hang it.
const BOUND: Duration = Duration::from_secs(10);

/// Polls until `cond` holds; after [`BOUND`] the child is killed and the test fails.
fn wait_for(child: &mut Child, what: &str, mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    while !cond() {
        if start.elapsed() > BOUND {
            let _ = child.kill();
            let _ = child.wait();
            panic!("timed out waiting for {what}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The child's stdout as a channel of lines (a reader thread), so reads can be bounded.
fn lines_of(stdout: ChildStdout) -> Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

/// The next line within [`BOUND`]; otherwise the child is killed and the test fails.
fn next_line(rx: &Receiver<String>, child: &mut Child, what: &str) -> String {
    match rx.recv_timeout(BOUND) {
        Ok(line) => line,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("no line within {BOUND:?} waiting for {what}: {e:?}");
        }
    }
}

fn pid_alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
        || Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .is_ok_and(|o| o.status.success())
}

/// A follow child for `session`, with the marker already written.
fn spawn_follow(h: &Home, session: Option<&str>) -> Child {
    let mut cmd = h.owl();
    cmd.args(["inbox", "--count", "--follow"]);
    if let Some(id) = session {
        cmd.args(["--session", id]);
    }
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(id) = session {
        let marker = h.marker(id);
        wait_for(&mut child, "the marker", || marker.exists());
        let pid: u32 = std::fs::read_to_string(&marker)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(pid, child.id(), "marker holds the follow's own pid");
        assert!(pid_alive(pid));
    }
    child
}

/// Waits for the child to end on its own within [`BOUND`]; a child still running then is
/// killed and the test fails (a follow whose exit path is broken must not hang the suite).
fn wait_exit(mut child: Child, what: &str) -> Output {
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > BOUND {
            let _ = child.kill();
            let _ = child.wait();
            panic!("follow still running after {BOUND:?}: {what}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

// ---------- AC3 ----------

/// The marker holds the pid; the counter goes out once per change, never repeated while the
/// count stands still, never at zero; nothing is marked seen; removing the marker ends the
/// loop cleanly.
#[test]
fn follow_prints_on_change_only_and_ends_when_its_marker_is_removed() {
    let h = Home::new();
    let mut child = spawn_follow(&h, Some("S1"));
    let rx = lines_of(child.stdout.take().unwrap());
    // 0 → 2: one counter line (the first thing ever printed: zero was silent).
    h.put("why does session 0 retry?");
    h.put("why does session 1 retry?");
    assert_eq!(
        next_line(&rx, &mut child, "the 2-count"),
        "🦉 owlpost: 2 new questions (Maciek 2). Say \"show owlpost inbox\" or run `owl inbox`."
    );
    // Many polls at the same count: nothing; 2 → 3: exactly the next line.
    std::thread::sleep(Duration::from_millis(400));
    h.put("and a third?");
    assert_eq!(
        next_line(&rx, &mut child, "the 3-count"),
        "🦉 owlpost: 3 new questions (Maciek 3). Say \"show owlpost inbox\" or run `owl inbox`."
    );
    // 3 → 0 is silent; 0 → 1 prints again, so the line after the 3-count is the 1-count.
    h.mark_all_seen();
    std::thread::sleep(Duration::from_millis(400));
    h.put("one more?");
    assert_eq!(
        next_line(&rx, &mut child, "the 1-count"),
        "🦉 owlpost: 1 new question (Maciek 1). Say \"show owlpost inbox\" or run `owl inbox`."
    );
    assert_eq!(h.unseen(), 1, "the follow never marks anything seen");
    // Removing the marker ends the loop (bounded wait): exit 0, nothing more on stdout,
    // marker still absent.
    std::fs::remove_file(h.marker("S1")).unwrap();
    let out = wait_exit(child, "after the marker was removed");
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(1)),
        Err(RecvTimeoutError::Disconnected),
        "no more output after the marker is gone"
    );
    assert_eq!(out.status.code(), Some(0), "{:?}", out.status);
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
    assert!(!h.marker("S1").exists());
}

/// A closed stdout (the Monitor's reader went away) ends the loop and removes the marker.
#[test]
fn follow_removes_its_marker_when_stdout_closes() {
    let h = Home::new();
    let mut child = spawn_follow(&h, Some("S2"));
    drop(child.stdout.take());
    h.put("anyone there?");
    let out = wait_exit(child, "follow to end on EPIPE");
    assert_eq!(out.status.code(), Some(0), "{:?}", out.status);
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
    assert!(!h.marker("S2").exists(), "marker removed on the clean end");
    assert_eq!(h.unseen(), 1);
}

/// Without `--session` the same loop runs with no marker at all.
#[test]
fn follow_without_session_writes_no_marker() {
    let h = Home::new();
    let mut child = spawn_follow(&h, None);
    h.put("hello?");
    let rx = lines_of(child.stdout.take().unwrap());
    assert!(next_line(&rx, &mut child, "the 1-count").starts_with("🦉 owlpost: 1 new question"));
    assert!(!h.path().join("watch").exists(), "no watch dir, no marker");
    child.kill().unwrap();
    child.wait().unwrap();
}

/// A killed follow leaves its marker behind (stale, pid dead); the hook path cleans it up.
#[test]
fn a_killed_follow_leaves_a_stale_marker_the_hook_removes() {
    let h = Home::new();
    let mut child = spawn_follow(&h, Some("S3"));
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(h.marker("S3").exists(), "kill leaves the marker");
    let out = h.hook(
        "UserPromptSubmit",
        Some(r#"{"session_id":"S3","hook_event_name":"UserPromptSubmit"}"#),
    );
    let ctx = context(&out).expect("arm sentence for a dead marker");
    assert!(ctx.contains("--session S3"), "{ctx}");
    assert!(
        !h.marker("S3").exists(),
        "stale marker removed by the hook path"
    );
}

/// The follow interval comes from `OWLPOST_FOLLOW_SECS`: with the default 5 s no line
/// appears within a second of a change, with 0.05 it does.
#[test]
fn follow_interval_is_read_from_the_env() {
    let h = Home::new();
    let mut child = Command::new(OWL)
        .env("OWLPOST_HOME", h.path())
        .env_remove("OWLPOST_FOLLOW_SECS")
        .args(["inbox", "--count", "--follow", "--session", "slow"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let marker = h.marker("slow");
    wait_for(&mut child, "the marker", || marker.exists());
    // The first poll ran at start (count 0, silent); the next one is 5 s away.
    std::thread::sleep(Duration::from_millis(100));
    h.put("slow?");
    std::thread::sleep(Duration::from_millis(900));
    child.kill().unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "",
        "5 s interval: no poll within a second"
    );
}

/// Ids outside `[A-Za-z0-9._-]{1,128}` are a clap usage error (exit 2) before anything is
/// written; so are `--session` without `--follow`, `--follow` without `--count`, and
/// `--follow` with `--format`.
#[test]
fn follow_rejects_bad_session_ids_and_flag_combinations_with_exit_2() {
    let h = Home::new();
    let long = "x".repeat(129);
    for bad in ["", "a b", "../x", "a/b", "ü", "$(x)", long.as_str()] {
        let out = h
            .owl()
            .args(["inbox", "--count", "--follow", "--session", bad])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{bad:?}: {:?}", out.status);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("session id must match [A-Za-z0-9._-]{1,128}"),
            "{bad:?}: {err}"
        );
        assert!(err.contains("for '--session <ID>'"), "{bad:?}: {err}");
        assert!(!h.path().join("watch").exists(), "{bad:?}: nothing written");
    }
    for args in [
        vec!["inbox", "--count", "--session", "S"],
        vec!["inbox", "--follow", "--session", "S"],
        vec![
            "inbox",
            "--count",
            "--follow",
            "--format",
            "plain",
            "--session",
            "S",
        ],
    ] {
        let out = h.owl().args(&args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}: {:?}", out.status);
        assert!(!h.path().join("watch").exists(), "{args:?}");
    }
    // The 128-char boundary is accepted (the follow starts and is killed at once).
    let ok = "y".repeat(128);
    let mut child = spawn_follow(&h, Some(&ok));
    child.kill().unwrap();
    child.wait().unwrap();
}

// ---------- AC4 ----------

/// `additionalContext` of a one-line `--format claude` output, `None` for empty output; the
/// line must be valid JSON with the event echoed.
fn context(out: &Output) -> Option<String> {
    assert_eq!(out.status.code(), Some(0), "{:?}", out.status);
    assert_eq!(String::from_utf8_lossy(&out.stderr), "", "stderr");
    let stdout = String::from_utf8(out.stdout.clone()).unwrap();
    if stdout.is_empty() {
        return None;
    }
    assert_eq!(stdout.lines().count(), 1, "one line: {stdout:?}");
    let v: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{e}: {stdout}"));
    Some(
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .to_string(),
    )
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Marker {
    None,
    Live,
    Dead,
}

/// The full grid: event × marker state × `plugin.json` × stdin, at 0 unseen. The sentence
/// goes out iff the watch is not off and (with `session_id` on stdin) the event is
/// `SessionStart` or `UserPromptSubmit` and no live marker exists — or (without stdin) the
/// event is `SessionStart`. A dead marker is removed by the arming events, a live one stays.
#[test]
fn hook_arms_per_event_marker_state_plugin_json_and_stdin() {
    let h = Home::new();
    let mut sleeper = Command::new("sleep")
        .arg("60")
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let mut reaped = Command::new("true").spawn().unwrap();
    reaped.wait().unwrap();
    std::fs::create_dir_all(h.path().join("watch")).unwrap();
    let plugin_json = h.path().join("plugin.json");
    let mut cells = 0;
    for event in EVENTS {
        for marker in [Marker::None, Marker::Live, Marker::Dead] {
            for watch in [None, Some(true), Some(false)] {
                for stdin in [Some(STDIN_S), None] {
                    match marker {
                        Marker::None => {
                            let _ = std::fs::remove_file(h.marker("S"));
                        }
                        Marker::Live => {
                            std::fs::write(h.marker("S"), sleeper.id().to_string()).unwrap()
                        }
                        Marker::Dead => {
                            std::fs::write(h.marker("S"), reaped.id().to_string()).unwrap()
                        }
                    }
                    match watch {
                        None => {
                            let _ = std::fs::remove_file(&plugin_json);
                        }
                        Some(b) => {
                            std::fs::write(&plugin_json, format!(r#"{{"watch": {b}}}"#)).unwrap()
                        }
                    }
                    let out = h.hook(event, stdin);
                    let ctx = context(&out);
                    let want = watch != Some(false)
                        && match stdin {
                            Some(_) => {
                                matches!(event, "SessionStart" | "UserPromptSubmit")
                                    && marker != Marker::Live
                            }
                            None => event == "SessionStart",
                        };
                    let cell = format!(
                        "{event} marker={marker:?} watch={watch:?} stdin={}",
                        stdin.is_some()
                    );
                    match (&ctx, want) {
                        (Some(ctx), true) => {
                            assert_eq!(
                                ctx,
                                if stdin.is_some() { ARM_S } else { ARM_BARE },
                                "{cell}"
                            );
                            assert!(ctx.starts_with(ARM_PREFIX), "{cell}");
                            assert!(ctx.contains("description \"owlpost inbox\""), "{cell}");
                            if stdin.is_some() {
                                assert!(ctx.contains("--session S"), "{cell}");
                            } else {
                                assert!(!ctx.contains("--session"), "{cell}");
                            }
                        }
                        (None, false) => {}
                        other => panic!("{cell}: got {other:?}"),
                    }
                    // Marker bookkeeping: a live marker always stays; a dead one is removed
                    // exactly when the arming path looked at it (session id + arming event).
                    let looked = stdin.is_some()
                        && matches!(event, "SessionStart" | "UserPromptSubmit")
                        && watch != Some(false);
                    match marker {
                        Marker::Live => assert!(h.marker("S").exists(), "{cell}: live marker kept"),
                        Marker::Dead => {
                            assert_eq!(!h.marker("S").exists(), looked, "{cell}: stale marker")
                        }
                        Marker::None => assert!(!h.marker("S").exists(), "{cell}"),
                    }
                    cells += 1;
                }
            }
        }
    }
    assert_eq!(cells, 3 * 3 * 3 * 2);
    sleeper.kill().unwrap();
    sleeper.wait().unwrap();
}

/// With unseen records the counter comes first and the arm sentence follows after one
/// space; on `SessionStart` the previews and the open sentence close the context. Nothing
/// is marked seen.
#[test]
fn hook_keeps_the_counter_and_previews_around_the_arm_sentence() {
    let h = Home::new();
    h.put("why does session 0 retry?");
    h.put("why does session 1 retry?");
    let counter =
        "🦉 owlpost: 2 new questions (Maciek 2). Say \"show owlpost inbox\" or run `owl inbox`.";
    let previews = "- Maciek question [pending] on src/auth/session.rs: why does session 0 retry?\n- Maciek question [pending] on src/auth/session.rs: why does session 1 retry?\nowlpost: run /owlpost:inbox now.";
    let ss = context(&h.hook("SessionStart", Some(STDIN_S))).unwrap();
    assert_eq!(ss, format!("{counter} {ARM_S}\n{previews}"));
    let ups = context(&h.hook("UserPromptSubmit", Some(STDIN_S))).unwrap();
    assert_eq!(ups, format!("{counter} {ARM_S}"));
    let ptu = context(&h.hook("PostToolUse", Some(STDIN_S))).unwrap();
    assert_eq!(ptu, counter);
    // Live marker: counter only, previews kept on SessionStart.
    std::fs::create_dir_all(h.path().join("watch")).unwrap();
    std::fs::write(h.marker("S"), std::process::id().to_string()).unwrap();
    assert_eq!(
        context(&h.hook("SessionStart", Some(STDIN_S))).unwrap(),
        format!("{counter}\n{previews}")
    );
    assert_eq!(
        context(&h.hook("UserPromptSubmit", Some(STDIN_S))).unwrap(),
        counter
    );
    // No stdin: the old rule, with the bare command.
    assert_eq!(
        context(&h.hook("SessionStart", None)).unwrap(),
        format!("{counter} {ARM_BARE}\n{previews}")
    );
    assert_eq!(context(&h.hook("UserPromptSubmit", None)).unwrap(), counter);
    assert_eq!(h.unseen(), 2);
    // The event name is echoed as hookEventName on every line.
    for event in EVENTS {
        let out = h.hook(event, Some(STDIN_S));
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], event);
    }
}

/// Stdin that carries no usable session id — empty, not JSON, no `session_id`, a bad id —
/// falls back to the old rule (`SessionStart` only, bare command) and touches no marker.
#[test]
fn hook_without_a_usable_session_id_uses_the_session_start_rule() {
    let h = Home::new();
    for stdin in [
        "",
        "not json",
        "{}",
        r#"{"session_id": 5}"#,
        r#"{"session_id": ""}"#,
        r#"{"session_id": "a/b"}"#,
        r#"{"sessionId": "S"}"#,
    ] {
        assert_eq!(
            context(&h.hook("SessionStart", Some(stdin))).as_deref(),
            Some(ARM_BARE),
            "{stdin:?}"
        );
        assert_eq!(
            context(&h.hook("UserPromptSubmit", Some(stdin))),
            None,
            "{stdin:?}"
        );
        assert_eq!(
            context(&h.hook("PostToolUse", Some(stdin))),
            None,
            "{stdin:?}"
        );
    }
    assert!(!h.path().join("watch").exists());
    // `--session-start` (the OWL-023 hooks.json) is accepted and ignored.
    let out = h
        .owl()
        .args([
            "inbox",
            "--count",
            "--format",
            "claude",
            "--hook-event",
            "SessionStart",
            "--session-start",
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(context(&out).as_deref(), Some(ARM_BARE));
    let out = h
        .owl()
        .args(["inbox", "--count", "--format", "claude", "--session-start"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(context(&out), None);
}

/// Other formats never read stdin for a session id and never carry the sentence.
#[test]
fn other_formats_ignore_stdin_and_markers() {
    let h = Home::new();
    h.put("one?");
    for f in ["plain", "codex", "kimi"] {
        for event in EVENTS {
            let mut child = h
                .owl()
                .args(["inbox", "--count", "--format", f, "--hook-event", event])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(STDIN_S.as_bytes())
                .unwrap();
            let out = child.wait_with_output().unwrap();
            let s = String::from_utf8_lossy(&out.stdout);
            assert!(s.contains("1 new question"), "{f} {event}: {s}");
            assert!(!s.contains(ARM_PREFIX), "{f} {event}: {s}");
            assert!(!s.contains("--follow"), "{f} {event}: {s}");
        }
    }
    assert!(!h.path().join("watch").exists());
}
