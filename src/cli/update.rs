//! `owl update [--source <dir>] [--dry-run]`: replace the `owl` binary (release installer, or
//! `cargo install` from a checkout), re-register the daemon so it runs the new binary, and
//! reinstall the Claude Code plugin so its copied command/hook files are refreshed.

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, bail};

use super::install;

const INSTALL_SH: &str = "https://raw.githubusercontent.com/Krab00/owlpost/main/scripts/install.sh";
const MARKETPLACE: &str = "owlpost-local";

fn binary_cmd(source: Option<&Path>) -> Vec<String> {
    match source {
        Some(dir) => vec![
            "cargo".into(),
            "install".into(),
            "--path".into(),
            dir.display().to_string(),
            "--locked".into(),
        ],
        None => vec![
            "sh".into(),
            "-c".into(),
            format!("f=$(mktemp) && curl -fsSL {INSTALL_SH} -o \"$f\" && sh \"$f\""),
        ],
    }
}

/// `claude plugin ...` steps; the marketplace refresh may fail (local path marketplaces have
/// nothing to fetch), the reinstall must not.
fn plugin_cmds() -> Vec<(bool, Vec<String>)> {
    let c = |args: &[&str]| {
        std::iter::once("claude".to_string())
            .chain(args.iter().map(|s| s.to_string()))
            .collect::<Vec<_>>()
    };
    vec![
        (false, c(&["plugin", "marketplace", "update", MARKETPLACE])),
        (
            true,
            c(&["plugin", "uninstall", &format!("owlpost@{MARKETPLACE}")]),
        ),
        (
            true,
            c(&[
                "plugin",
                "install",
                &format!("owlpost@{MARKETPLACE}"),
                "--scope",
                "user",
            ]),
        ),
    ]
}

fn run(cmd: &[String], required: bool) -> anyhow::Result<()> {
    println!("+ {}", cmd.join(" "));
    let status = Command::new(&cmd[0])
        .args(&cmd[1..])
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("running {}", cmd[0]))?;
    if !status.success() && required {
        bail!("{} failed ({status})", cmd.join(" "));
    }
    Ok(())
}

pub fn run_update(home: &Path, source: Option<&Path>, dry_run: bool) -> anyhow::Result<()> {
    let has_claude = Command::new("claude")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if dry_run {
        println!("would run: {}", binary_cmd(source).join(" "));
        println!("would run: owl uninstall && owl install");
        if has_claude {
            for (_, c) in plugin_cmds() {
                println!("would run: {}", c.join(" "));
            }
        } else {
            println!("claude not on PATH: plugin reinstall skipped");
        }
        return Ok(());
    }
    run(&binary_cmd(source), true)?;
    // ponytail: uninstall+install re-points the unit at the current binary path; a symlinked
    // `owl` replaced by the installer keeps the old target until the next update.
    install::uninstall()?;
    install::install(home, false)?;
    if has_claude {
        for (required, c) in plugin_cmds() {
            run(&c, required)?;
        }
    } else {
        println!("claude not on PATH: plugin reinstall skipped");
    }
    println!("updated; restart your Claude Code session to load the plugin");
    Ok(())
}
