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
fn whoami_with_config_and_key_is_not_implemented_yet() {
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
    assert!(err.contains("not implemented yet"), "stderr: {err}");
    assert!(!err.contains("owl init"), "stderr: {err}");
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
