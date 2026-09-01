use std::process::Command;

fn owl() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_owl"));
    c.env_remove("OWLPOST_HOME");
    c
}

#[test]
fn version() {
    let out = owl().arg("--version").output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "owl 0.1.0");
}

#[test]
fn help_lists_all_subcommands() {
    let out = owl().arg("--help").output().unwrap();
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    // Parse the "Commands:" block: first token of each indented line up to the blank line.
    let listed: Vec<&str> = help
        .split("Commands:\n")
        .nth(1)
        .expect("Commands: block")
        .lines()
        .take_while(|l| !l.trim().is_empty())
        .filter_map(|l| l.split_whitespace().next())
        .filter(|t| *t != "help")
        .collect();
    let expected = [
        "init",
        "whoami",
        "card",
        "contact",
        "add",
        "allow",
        "deny",
        "ask",
        "inbox",
        "show",
        "draft",
        "edit",
        "send",
        "reject",
        "history",
        "watch",
        "daemon",
        "install",
        "uninstall",
        "doctor",
    ];
    assert_eq!(listed, expected, "help:\n{help}");
    for flag in ["--home", "--json", "--quiet"] {
        assert!(help.contains(flag), "missing global flag {flag}");
    }
}

#[test]
fn whoami_on_empty_home_asks_for_init() {
    let home = tempfile::tempdir().unwrap();
    let out = owl()
        .args(["--home"])
        .arg(home.path())
        .arg("whoami")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("owl init"), "stderr: {err}");
    assert!(out.stdout.is_empty());
}

#[test]
fn whoami_honours_owlpost_home_env() {
    let home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_owl"))
        .env("OWLPOST_HOME", home.path())
        .arg("whoami")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("owl init"));
}

#[test]
fn whoami_with_config_but_no_key_asks_for_init() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.json"), "{}").unwrap();
    let out = owl()
        .args(["--home"])
        .arg(home.path())
        .arg("whoami")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("owl init"));
}

#[test]
fn whoami_with_key_but_no_config_asks_for_init() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("key"), "k").unwrap();
    let out = owl()
        .args(["--home"])
        .arg(home.path())
        .arg("whoami")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("owl init"));
}

#[test]
fn whoami_with_config_and_garbage_key_fails_without_init_hint() {
    // OWL-001 asserted "not implemented yet" here; since OWL-002 whoami is real and a
    // 1-byte key is a load error, not a missing identity.
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.json"), "{}").unwrap();
    std::fs::write(home.path().join("key"), "k").unwrap();
    let out = owl()
        .args(["--home"])
        .arg(home.path())
        .arg("whoami")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("32 bytes"), "stderr: {err}");
    assert!(!err.contains("owl init"), "stderr: {err}");
}

fn run_init(home: &std::path::Path) -> std::process::Output {
    owl()
        .args(["--home"])
        .arg(home)
        .args(["init", "--name", "Test", "--email", "t@example.org"])
        .output()
        .unwrap()
}

fn is_fingerprint(s: &str) -> bool {
    // ^owl:[a-z2-7]{16}$
    s.len() == 20
        && s.starts_with("owl:")
        && s[4..]
            .bytes()
            .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
}

#[test]
fn init_creates_key_and_config_and_prints_fingerprint() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path().join("fresh"); // init must create the home dir
    let out = run_init(&home);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "stdout: {stdout}");
    assert!(is_fingerprint(lines[0]), "stdout: {stdout}");
    assert_eq!(std::fs::read(home.join("key")).unwrap().len(), 32);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(home.join("key"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let cfg: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.join("config.json")).unwrap()).unwrap();
    assert_eq!(cfg["name"], "Test");
    assert_eq!(cfg["emails"], serde_json::json!(["t@example.org"]));
}

#[test]
fn init_accepts_repeated_email_and_no_name() {
    let home = tempfile::tempdir().unwrap();
    let out = owl()
        .args(["--home"])
        .arg(home.path())
        .args(["init", "--email", "a@x.org", "--email", "b@x.org"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let cfg: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.path().join("config.json")).unwrap()).unwrap();
    assert_eq!(cfg["name"], "");
    assert_eq!(cfg["emails"], serde_json::json!(["a@x.org", "b@x.org"]));
}

#[test]
fn init_twice_refuses_and_keeps_key() {
    let home = tempfile::tempdir().unwrap();
    assert!(run_init(home.path()).status.success());
    let key = std::fs::read(home.path().join("key")).unwrap();
    let cfg = std::fs::read(home.path().join("config.json")).unwrap();
    let out = owl()
        .args(["--home"])
        .arg(home.path())
        .args(["init", "--name", "Other"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("already exists"), "stderr: {err}");
    assert!(out.stdout.is_empty());
    assert_eq!(std::fs::read(home.path().join("key")).unwrap(), key);
    assert_eq!(
        std::fs::read(home.path().join("config.json")).unwrap(),
        cfg,
        "config untouched"
    );
}

#[test]
fn whoami_json_after_init() {
    let home = tempfile::tempdir().unwrap();
    let init = run_init(home.path());
    assert!(init.status.success());
    let fp = String::from_utf8_lossy(&init.stdout).trim().to_string();
    let out = owl()
        .args(["--home"])
        .arg(home.path())
        .args(["whoami", "--json"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let obj = v.as_object().unwrap();
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["endpoints", "fingerprint", "name", "pubkey"]);
    assert_eq!(v["fingerprint"], fp);
    assert_eq!(v["name"], "Test");
    assert_eq!(v["endpoints"], serde_json::json!([]));
    let pk = v["pubkey"].as_str().unwrap();
    assert!(pk.starts_with("ed25519:"), "{pk}");
    assert_eq!(pk.len(), 8 + 43, "{pk}");
    assert!(!pk.ends_with('='), "{pk}");
}

#[test]
fn whoami_text_after_init() {
    let home = tempfile::tempdir().unwrap();
    let init = run_init(home.path());
    let fp = String::from_utf8_lossy(&init.stdout).trim().to_string();
    let out = owl()
        .args(["--home"])
        .arg(home.path())
        .arg("whoami")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&format!("fingerprint: {fp}\n")),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("pubkey: ed25519:"), "stdout: {stdout}");
    assert!(stdout.contains("name: Test\n"), "stdout: {stdout}");
    assert!(stdout.contains("endpoints: "), "stdout: {stdout}");
}

#[test]
fn unimplemented_subcommand_exits_1() {
    let home = tempfile::tempdir().unwrap();
    let out = owl()
        .args(["--home"])
        .arg(home.path())
        .arg("doctor")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not implemented yet"));
}

#[test]
fn unknown_subcommand_is_usage_error() {
    let out = owl().arg("bogus").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}
