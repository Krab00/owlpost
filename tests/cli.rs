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
    for name in [
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
    ] {
        assert!(
            help.lines().any(|l| l.trim_start().starts_with(name)),
            "missing subcommand {name} in help:\n{help}"
        );
    }
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
