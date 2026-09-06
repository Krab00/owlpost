//! OWL-025 AC2/AC3: `owl setup` and `owl update` end their Claude Code steps with the
//! idempotent user-scope registration of the `owl` MCP server. A fake `claude` (and `cargo`)
//! on a PATH of its own logs every argv; `claude mcp get owl` exits per case.

mod common;

use std::path::Path;
use std::process::{Command, Output};

use common::{fake_calls, fake_claude};

const MCP_GET: &str = "claude mcp get owl";
const MCP_ADD: &str = "claude mcp add --scope user owl -- owl mcp";

/// `owl` with `$OWLPOST_HOME` = `home`, `HOME` = `user_home` (the systemd unit lands there,
/// `OWLPOST_INSTALL_NO_LOAD` keeps `systemctl` out of it) and `PATH` = exactly `bin`.
fn owl(home: &Path, user_home: &Path, bin: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_owl"));
    c.env_remove("OWLPOST_HOME")
        .env("HOME", user_home)
        .env("OWLPOST_INSTALL_NO_LOAD", "1")
        .env("PATH", bin)
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

/// One cell of the matrix: runs `args` with a fake `claude` whose `mcp get owl` exits
/// `get_exit`; returns (stdout, log lines).
fn run(args: &[&str], get_exit: i32) -> (String, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("owlpost");
    let user_home = dir.path().join("user");
    std::fs::create_dir_all(&user_home).unwrap();
    let (bin, log) = fake_claude(dir.path(), get_exit);
    let out = owl(&home, &user_home, &bin).args(args).output().unwrap();
    let (stdout, stderr) = text(&out);
    assert!(
        out.status.success(),
        "{args:?}: exit {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    (stdout, fake_calls(&log))
}

const SETUP_PLUGIN: [&str; 2] = [
    "claude plugin marketplace add /repo/plugins/claude-code",
    "claude plugin install owlpost@owlpost-local --scope user",
];
const UPDATE_PLUGIN: [&str; 3] = [
    "claude plugin marketplace update owlpost-local",
    "claude plugin uninstall owlpost@owlpost-local",
    "claude plugin install owlpost@owlpost-local --scope user",
];

/// `claude --version` (the PATH probe) runs first, then the binary step (`owl update`
/// only), the plugin steps, and the `mcp` tail.
fn expected(binary: &[&str], plugin: &[&str], tail: &[&str]) -> Vec<String> {
    std::iter::once("claude --version")
        .chain(binary.iter().copied())
        .chain(plugin.iter().copied())
        .chain(tail.iter().copied())
        .map(str::to_string)
        .collect()
}

const UPDATE_BINARY: [&str; 1] = ["cargo install --path /repo --locked"];

const SETUP: [&str; 3] = ["setup", "--plugin-source", "/repo/plugins/claude-code"];
const UPDATE: [&str; 3] = ["update", "--source", "/repo"];

#[test]
fn setup_registers_owl_mcp_after_the_plugin_steps() {
    let (stdout, calls) = run(&SETUP, 1);
    assert_eq!(
        calls,
        expected(&[], &SETUP_PLUGIN, &[MCP_GET, MCP_ADD]),
        "{stdout}"
    );
    assert!(stdout.contains(&format!("+ {MCP_ADD}\n")), "{stdout}");
    assert!(!stdout.contains("already registered"), "{stdout}");
    // The identity and the daemon unit were created before the Claude Code steps.
    assert!(stdout.contains("installed "), "{stdout}");
}

#[test]
fn setup_skips_registration_when_owl_mcp_is_registered() {
    let (stdout, calls) = run(&SETUP, 0);
    assert_eq!(calls, expected(&[], &SETUP_PLUGIN, &[MCP_GET]), "{stdout}");
    assert!(
        stdout.contains("mcp server owl already registered\n"),
        "{stdout}"
    );
    assert!(!stdout.contains("mcp add"), "{stdout}");
}

#[test]
fn setup_dry_run_prints_the_registration_and_runs_nothing() {
    let (stdout, calls) = run(&[SETUP[0], SETUP[1], SETUP[2], "--dry-run"], 1);
    // Only the read-only probes ran: no plugin step, no `mcp add`.
    assert_eq!(calls, ["claude --version", MCP_GET], "{stdout}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        [
            "would run: owl init",
            "would run: owl install",
            "would run: claude plugin marketplace add /repo/plugins/claude-code",
            "would run: claude plugin install owlpost@owlpost-local --scope user",
            "would run: claude mcp add --scope user owl -- owl mcp",
        ]
    );
    let (stdout, calls) = run(&[SETUP[0], SETUP[1], SETUP[2], "--dry-run"], 0);
    assert_eq!(calls, ["claude --version", MCP_GET], "{stdout}");
    assert!(
        stdout.ends_with("mcp server owl already registered\n"),
        "{stdout}"
    );
    assert!(!stdout.contains("mcp add"), "{stdout}");
}

#[test]
fn update_registers_owl_mcp_after_the_plugin_steps() {
    let (stdout, calls) = run(&UPDATE, 1);
    assert_eq!(
        calls,
        expected(&UPDATE_BINARY, &UPDATE_PLUGIN, &[MCP_GET, MCP_ADD]),
        "{stdout}"
    );
    assert!(stdout.contains(&format!("+ {MCP_ADD}\n")), "{stdout}");
    assert!(!stdout.contains("already registered"), "{stdout}");
}

#[test]
fn update_skips_registration_when_owl_mcp_is_registered() {
    let (stdout, calls) = run(&UPDATE, 0);
    assert_eq!(
        calls,
        expected(&UPDATE_BINARY, &UPDATE_PLUGIN, &[MCP_GET]),
        "{stdout}"
    );
    assert!(
        stdout.contains("mcp server owl already registered\n"),
        "{stdout}"
    );
    assert!(!stdout.contains("mcp add"), "{stdout}");
}

#[test]
fn update_dry_run_prints_the_registration_and_runs_nothing() {
    let (stdout, calls) = run(&[UPDATE[0], UPDATE[1], UPDATE[2], "--dry-run"], 1);
    assert_eq!(calls, ["claude --version", MCP_GET], "{stdout}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        [
            "would run: cargo install --path /repo --locked",
            "would run: owl uninstall && owl install",
            "would run: claude plugin marketplace update owlpost-local",
            "would run: claude plugin uninstall owlpost@owlpost-local",
            "would run: claude plugin install owlpost@owlpost-local --scope user",
            "would run: claude mcp add --scope user owl -- owl mcp",
        ]
    );
    let (stdout, calls) = run(&[UPDATE[0], UPDATE[1], UPDATE[2], "--dry-run"], 0);
    assert_eq!(calls, ["claude --version", MCP_GET], "{stdout}");
    assert!(
        stdout.ends_with("mcp server owl already registered\n"),
        "{stdout}"
    );
    assert!(!stdout.contains("mcp add"), "{stdout}");
}

/// Without `claude` the registration is skipped with the plugin steps, in both modes.
#[test]
fn no_claude_on_path_skips_the_registration() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("owlpost");
    let user_home = dir.path().join("user");
    std::fs::create_dir_all(&user_home).unwrap();
    let empty = dir.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    for args in [&SETUP[..], &UPDATE[..]] {
        let out = owl(&home, &user_home, &empty)
            .args(args)
            .arg("--dry-run")
            .output()
            .unwrap();
        let (stdout, stderr) = text(&out);
        assert!(out.status.success(), "{args:?}: {stderr}");
        assert!(stdout.contains("claude not on PATH"), "{stdout}");
        assert!(!stdout.contains("mcp"), "{stdout}");
    }
}
