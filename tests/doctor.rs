//! AC5: `owl doctor` prints one line per check and exits 0 with a running daemon, exits 1 with
//! `fail` on the daemon line once the daemon is stopped.

mod common;

use std::path::Path;
use std::process::{Command, Output};

use owlpost::daemon;

use common::{spawn_daemon_with, write_contact};

fn owl(home: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_owl"));
    c.env_remove("OWLPOST_HOME").arg("--home").arg(home);
    c
}

fn text(o: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

/// The `<status> <name>:` prefix of every line, in order.
fn heads(stdout: &str) -> Vec<(String, String)> {
    stdout
        .lines()
        .map(|l| {
            let (status, rest) = l.split_once(' ').expect("status");
            let name = rest.trim_start().split_once(':').expect("name").0;
            (status.to_string(), name.to_string())
        })
        .collect()
}

fn line<'a>(stdout: &'a str, name: &str) -> &'a str {
    stdout
        .lines()
        .find(|l| l.split_whitespace().nth(1) == Some(&format!("{name}:")))
        .unwrap_or_else(|| panic!("no {name} line in:\n{stdout}"))
}

/// Only the `fake` harness is configured, so the check passes offline on any machine.
fn config_for_doctor(cfg: &mut owlpost::config::Config) {
    cfg.harnesses.retain(|k, _| k == "fake");
    cfg.responder.harness = "fake".into();
    cfg.endpoints = vec!["127.0.0.1:7411".into()];
}

#[tokio::test(flavor = "multi_thread")]
async fn doctor_reports_each_check() {
    let d = spawn_daemon_with(1, &[], config_for_doctor).await;
    daemon::write_addr_file(d.home(), d.addr).unwrap();

    let out = owl(d.home()).arg("doctor").output().unwrap();
    let (stdout, stderr) = text(&out);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr: {stderr}"
    );
    assert_eq!(
        heads(&stdout),
        [
            ("ok", "key"),
            ("ok", "config"),
            ("ok", "endpoints"),
            ("ok", "harness"),
            ("ok", "daemon"),
            ("warn", "pull"),
        ]
        .map(|(s, n)| (s.to_string(), n.to_string())),
        "{stdout}"
    );
    assert!(line(&stdout, "key").contains(&d.fp()), "{stdout}");
    assert!(line(&stdout, "config").contains("config.json"), "{stdout}");
    assert!(
        line(&stdout, "endpoints").contains("127.0.0.1:7411 -> 127.0.0.1"),
        "{stdout}"
    );
    assert!(line(&stdout, "harness").contains("fake: "), "{stdout}");
    assert!(
        line(&stdout, "harness").contains("fake-harness.sh"),
        "{stdout}"
    );
    assert!(
        line(&stdout, "daemon").contains(&format!("reachable at {}", d.addr)),
        "{stdout}"
    );
    assert!(line(&stdout, "daemon").contains(&d.fp()), "{stdout}");
    assert!(line(&stdout, "pull").contains("never pulled"), "{stdout}");
    assert!(stderr.is_empty(), "{stderr}");

    // A recorded pull turns the last line ok.
    std::fs::write(d.home().join("last-pull"), "").unwrap();
    let out = owl(d.home()).arg("doctor").output().unwrap();
    let (stdout, _) = text(&out);
    assert_eq!(out.status.code(), Some(0));
    assert!(
        line(&stdout, "pull").starts_with("ok   pull: last pull "),
        "{stdout}"
    );

    // --json: one object per check with the same content.
    let out = owl(d.home()).args(["--json", "doctor"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let json: serde_json::Value = serde_json::from_str(&text(&out).0).unwrap();
    let arr = json.as_array().expect("array");
    let names: Vec<&str> = arr
        .iter()
        .map(|c| c.get("check").and_then(|v| v.as_str()).unwrap())
        .collect();
    assert_eq!(
        names,
        ["key", "config", "endpoints", "harness", "daemon", "pull"]
    );
    assert!(
        arr.iter()
            .all(|c| c.get("status").and_then(|v| v.as_str()) == Some("ok"))
    );
    assert!(
        arr[4]
            .get("detail")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.contains("reachable")),
        "{json}"
    );

    // Daemon stopped, nothing else changed: exit 1, fail on the daemon line only.
    d.running.shutdown();
    let addr = d.addr;
    let common::TestDaemon { dir, running, .. } = d;
    running.wait().await.unwrap();
    let home = dir.path();
    let out = owl(home).arg("doctor").output().unwrap();
    let (stdout, stderr) = text(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "stdout:\n{stdout}\nstderr: {stderr}"
    );
    assert_eq!(
        heads(&stdout),
        [
            ("ok", "key"),
            ("ok", "config"),
            ("ok", "endpoints"),
            ("ok", "harness"),
            ("fail", "daemon"),
            ("ok", "pull"),
        ]
        .map(|(s, n)| (s.to_string(), n.to_string())),
        "{stdout}"
    );
    assert!(
        line(&stdout, "daemon").contains(&addr.to_string()),
        "{stdout}"
    );
    assert!(stderr.contains("1 check(s) failed"), "{stderr}");
    // --json carries the failure too, still exit 1.
    let out = owl(home).args(["--json", "doctor"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let json: serde_json::Value = serde_json::from_str(&text(&out).0).unwrap();
    assert_eq!(
        json.get(4)
            .and_then(|c| c.get("status"))
            .and_then(|v| v.as_str()),
        Some("fail")
    );

    // No daemon.addr at all: fail names the missing file.
    std::fs::remove_file(home.join("daemon.addr")).unwrap();
    let out = owl(home).arg("doctor").output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(line(&text(&out).0, "daemon").contains("daemon.addr missing"));
}

#[tokio::test(flavor = "multi_thread")]
async fn doctor_fails_on_foreign_daemon_and_missing_pieces() {
    // daemon.addr points at somebody else's daemon: reachable, but not ours (pin mismatch).
    let mine = spawn_daemon_with(1, &[], config_for_doctor).await;
    let other = spawn_daemon_with(2, &[], config_for_doctor).await;
    daemon::write_addr_file(mine.home(), other.addr).unwrap();
    let out = owl(mine.home()).arg("doctor").output().unwrap();
    let (stdout, _) = text(&out);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    let daemon_line = line(&stdout, "daemon");
    assert!(daemon_line.starts_with("fail"), "{stdout}");
    // Our own key is the TLS pin, so a foreign daemon is refused during the handshake.
    assert!(daemon_line.contains("invalid peer certificate"), "{stdout}");
    assert!(daemon_line.contains(&other.addr.to_string()), "{stdout}");
    mine.running.shutdown();
    other.running.shutdown();

    // Empty home: key fails, config warns (defaults), endpoints warn, harness lines for the
    // default set, daemon fails, pull warns — and the harness line for `fake` still resolves.
    let empty = tempfile::tempdir().unwrap();
    let out = owl(empty.path()).arg("doctor").output().unwrap();
    let (stdout, _) = text(&out);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(line(&stdout, "key").starts_with("fail"), "{stdout}");
    assert!(line(&stdout, "config").starts_with("warn"), "{stdout}");
    assert!(line(&stdout, "endpoints").starts_with("warn"), "{stdout}");
    assert!(
        line(&stdout, "endpoints").contains("none configured"),
        "{stdout}"
    );
    assert!(
        stdout
            .lines()
            .any(|l| l.starts_with("ok   harness: fake: ")),
        "{stdout}"
    );
    assert!(line(&stdout, "daemon").starts_with("fail"), "{stdout}");
    assert!(line(&stdout, "pull").starts_with("warn"), "{stdout}");

    // Broken config → fail on the config line; an unresolvable endpoint → fail.
    let broken = tempfile::tempdir().unwrap();
    std::fs::write(broken.path().join("config.json"), "{nope").unwrap();
    let out = owl(broken.path()).arg("doctor").output().unwrap();
    let (stdout, _) = text(&out);
    assert!(line(&stdout, "config").starts_with("fail"), "{stdout}");
    assert!(line(&stdout, "config").contains("parsing"), "{stdout}");
    let mut cfg = owlpost::config::Config::default();
    config_for_doctor(&mut cfg);
    cfg.endpoints = vec!["noport".into()];
    cfg.save(broken.path()).unwrap();
    write_contact(broken.path(), &common::Peer::new(&common::id(3), "X", None));
    let out = owl(broken.path()).arg("doctor").output().unwrap();
    let (stdout, _) = text(&out);
    assert!(line(&stdout, "config").starts_with("ok"), "{stdout}");
    assert!(
        line(&stdout, "endpoints").starts_with("fail endpoints: noport: "),
        "{stdout}"
    );
}
