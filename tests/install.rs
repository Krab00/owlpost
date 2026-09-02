//! AC3 (`install --dry-run` prints the unit, writes nothing) and AC6 (`install` / `uninstall`
//! create and remove the unit file under `$HOME` with loading skipped).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn owl(home: &Path, user_home: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_owl"));
    c.env_remove("OWLPOST_HOME")
        .env_remove("OWLPOST_INSTALL_NO_LOAD")
        .env("HOME", user_home)
        .arg("--home")
        .arg(home);
    c
}

fn text(o: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

/// Where the current OS keeps the unit under `user_home`.
fn unit_path(user_home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        user_home.join("Library/LaunchAgents/dev.owlpost.owl.plist")
    } else {
        user_home.join(".config/systemd/user/owlpost.service")
    }
}

fn owl_binary() -> String {
    std::fs::canonicalize(env!("CARGO_BIN_EXE_owl"))
        .unwrap()
        .display()
        .to_string()
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(files_under(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

fn assert_unit(unit: &str, home: &Path) {
    assert!(unit.contains(&owl_binary()), "absolute owl path:\n{unit}");
    assert!(unit.contains("daemon"), "{unit}");
    assert!(unit.contains("--home"), "{unit}");
    assert!(unit.contains(&home.display().to_string()), "{unit}");
    if cfg!(target_os = "macos") {
        assert!(unit.starts_with("<?xml"), "{unit}");
        assert!(unit.contains("<string>dev.owlpost.owl</string>"), "{unit}");
        assert!(unit.contains(&format!(
            "<string>{}</string>\n        <string>daemon</string>\n        <string>--home</string>\n        <string>{}</string>",
            owl_binary(),
            home.display()
        )));
    } else {
        assert!(unit.starts_with("[Unit]"), "{unit}");
        assert!(unit.contains(&format!(
            "ExecStart=\"{}\" daemon --home \"{}\"",
            owl_binary(),
            home.display()
        )));
        assert!(unit.contains("WantedBy=default.target"), "{unit}");
    }
}

#[test]
fn install_dry_run_prints_unit_and_writes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let user_home = tempfile::tempdir().unwrap();
    let out = owl(home.path(), user_home.path())
        .args(["install", "--dry-run"])
        .output()
        .unwrap();
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "stderr: {stderr}");
    assert_unit(&stdout, home.path());
    assert!(
        !stdout.contains("installed"),
        "dry run must not report a write"
    );
    assert!(
        files_under(user_home.path()).is_empty(),
        "dry run wrote {:?}",
        files_under(user_home.path())
    );
    assert!(!unit_path(user_home.path()).exists());
    assert!(
        files_under(home.path()).is_empty(),
        "dry run touched the owlpost home"
    );
}

#[test]
fn install_dry_run_uses_the_given_home() {
    // Two different homes produce two different units: the flag is not ignored.
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let user_home = tempfile::tempdir().unwrap();
    let unit_a = text(
        &owl(a.path(), user_home.path())
            .args(["install", "--dry-run"])
            .output()
            .unwrap(),
    )
    .0;
    let unit_b = text(
        &owl(b.path(), user_home.path())
            .args(["install", "--dry-run"])
            .output()
            .unwrap(),
    )
    .0;
    assert_ne!(unit_a, unit_b);
    assert!(unit_a.contains(&a.path().display().to_string()));
    assert!(!unit_a.contains(&b.path().display().to_string()));
    assert!(unit_b.contains(&b.path().display().to_string()));
    // A relative --home is made absolute in the unit.
    let cwd = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_owl"))
        .env_remove("OWLPOST_HOME")
        .env("HOME", user_home.path())
        .current_dir(cwd.path())
        .args(["--home", "rel-home", "install", "--dry-run"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let unit = text(&out).0;
    assert!(
        unit.contains(&cwd.path().join("rel-home").display().to_string()),
        "{unit}"
    );
}

#[test]
fn install_then_uninstall_creates_and_removes_unit_file() {
    let home = tempfile::tempdir().unwrap();
    let user_home = tempfile::tempdir().unwrap();
    let unit = unit_path(user_home.path());

    let out = owl(home.path(), user_home.path())
        .env("OWLPOST_INSTALL_NO_LOAD", "1")
        .arg("install")
        .output()
        .unwrap();
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "install: {stderr}");
    assert!(unit.is_file(), "unit file missing at {}", unit.display());
    assert!(
        stdout.contains(&format!("installed {}", unit.display())),
        "{stdout}"
    );
    assert!(stdout.contains("load skipped"), "{stdout}");
    assert!(!stdout.contains("started"), "{stdout}");
    assert!(
        !unit.with_extension("tmp").exists(),
        "temp file left behind"
    );
    let content = std::fs::read_to_string(&unit).unwrap();
    assert_unit(&content, home.path());
    // The written unit equals the dry-run output.
    let dry = text(
        &owl(home.path(), user_home.path())
            .args(["install", "--dry-run"])
            .output()
            .unwrap(),
    )
    .0;
    assert_eq!(content, dry);

    // Installing again overwrites in place (a different home lands in the file).
    let home2 = tempfile::tempdir().unwrap();
    let out = owl(home2.path(), user_home.path())
        .env("OWLPOST_INSTALL_NO_LOAD", "1")
        .arg("install")
        .output()
        .unwrap();
    assert!(out.status.success());
    let content = std::fs::read_to_string(&unit).unwrap();
    assert!(content.contains(&home2.path().display().to_string()));
    assert!(!content.contains(&home.path().display().to_string()));

    let out = owl(home.path(), user_home.path())
        .env("OWLPOST_INSTALL_NO_LOAD", "1")
        .arg("uninstall")
        .output()
        .unwrap();
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "uninstall: {stderr}");
    assert!(!unit.exists(), "unit file still present");
    assert!(stdout.contains("stop skipped"), "{stdout}");
    assert!(
        stdout.contains(&format!("removed {}", unit.display())),
        "{stdout}"
    );

    // Uninstalling again is a no-op that says so.
    let out = owl(home.path(), user_home.path())
        .env("OWLPOST_INSTALL_NO_LOAD", "1")
        .arg("uninstall")
        .output()
        .unwrap();
    let (stdout, _) = text(&out);
    assert!(out.status.success());
    assert!(stdout.contains("not installed"), "{stdout}");
}

#[test]
fn install_blocked_target_is_a_clean_error() {
    let home = tempfile::tempdir().unwrap();
    let user_home = tempfile::tempdir().unwrap();
    let unit = unit_path(user_home.path());
    std::fs::create_dir_all(unit.join("child")).unwrap();
    let out = owl(home.path(), user_home.path())
        .env("OWLPOST_INSTALL_NO_LOAD", "1")
        .arg("install")
        .output()
        .unwrap();
    let (stdout, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr.contains("owl: "), "{stderr}");
    assert!(stderr.contains("renaming to"), "{stderr}");
    assert!(!stdout.contains("installed"), "{stdout}");
    assert!(unit.is_dir(), "blocker untouched");
    assert!(!unit.with_extension("tmp").exists(), "temp file cleaned up");
}

#[test]
fn install_without_home_env_fails() {
    let home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_owl"))
        .env_remove("OWLPOST_HOME")
        .env_remove("HOME")
        .env("OWLPOST_INSTALL_NO_LOAD", "1")
        .arg("--home")
        .arg(home.path())
        .arg("install")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out).1.contains("HOME is not set"), "{}", text(&out).1);
    // dry-run needs no HOME at all.
    let out = Command::new(env!("CARGO_BIN_EXE_owl"))
        .env_remove("OWLPOST_HOME")
        .env_remove("HOME")
        .arg("--home")
        .arg(home.path())
        .args(["install", "--dry-run"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out).1);
}
