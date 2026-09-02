//! `owl install [--dry-run]` / `owl uninstall` (§11): a launchd agent on macOS, a systemd user
//! unit on Linux, running `owl daemon --home <home>` with the absolute path of this binary.
//!
//! `HOME` resolves `~` (tests point it at a temp dir); `OWLPOST_INSTALL_NO_LOAD=1` skips the
//! `launchctl` / `systemctl` step so the file operations can be tested without a service
//! manager.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

/// launchd label / plist basename.
pub const LAUNCHD_LABEL: &str = "dev.owlpost.owl";
/// systemd unit name.
pub const SYSTEMD_UNIT: &str = "owlpost.service";
/// When set (to anything non-empty), install/uninstall only touch the unit file.
pub const NO_LOAD_ENV: &str = "OWLPOST_INSTALL_NO_LOAD";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    MacOs,
    Linux,
}

impl Os {
    /// The host OS; an error elsewhere (no service manager to write for).
    pub fn current() -> anyhow::Result<Os> {
        if cfg!(target_os = "macos") {
            Ok(Os::MacOs)
        } else if cfg!(target_os = "linux") {
            Ok(Os::Linux)
        } else {
            bail!("owl install supports macOS (launchd) and Linux (systemd --user) only")
        }
    }
}

/// Where the unit lives under the user's home directory.
pub fn unit_path(os: Os, user_home: &Path) -> PathBuf {
    match os {
        Os::MacOs => user_home
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{LAUNCHD_LABEL}.plist")),
        Os::Linux => user_home
            .join(".config")
            .join("systemd")
            .join("user")
            .join(SYSTEMD_UNIT),
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// systemd `ExecStart` quoting: double quotes, with backslash and quote escaped.
fn systemd_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The service definition: a plist for launchd, a `[Service]` unit for systemd. Pure, so
/// both flavours are unit-tested on any host.
pub fn unit_text(os: Os, owl_path: &Path, home: &Path) -> String {
    let owl = owl_path.display().to_string();
    let home_s = home.display().to_string();
    match os {
        Os::MacOs => {
            let log = xml_escape(&home.join("daemon.log").display().to_string());
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCHD_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{owl}</string>
        <string>daemon</string>
        <string>--home</string>
        <string>{home}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#,
                owl = xml_escape(&owl),
                home = xml_escape(&home_s),
            )
        }
        Os::Linux => format!(
            "[Unit]\n\
             Description=owlpost daemon (owl daemon)\n\
             After=network-online.target\n\
             \n\
             [Service]\n\
             ExecStart={} daemon --home {}\n\
             Restart=on-failure\n\
             RestartSec=5\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n",
            systemd_quote(&owl),
            systemd_quote(&home_s),
        ),
    }
}

fn user_home() -> anyhow::Result<PathBuf> {
    match std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        Some(h) => Ok(PathBuf::from(h)),
        None => bail!("HOME is not set; cannot locate the service directory"),
    }
}

fn no_load() -> bool {
    std::env::var_os(NO_LOAD_ENV).is_some_and(|v| !v.is_empty())
}

/// Absolute path of the running `owl` binary.
pub fn owl_path() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe().context("locating the owl binary")?;
    Ok(std::fs::canonicalize(&exe).unwrap_or(exe))
}

/// Atomic write (temp + rename) creating parent directories.
pub fn write_unit(path: &Path, text: &str) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("renaming to {}", path.display()));
    }
    Ok(())
}

fn run(program: &str, args: &[String]) -> anyhow::Result<()> {
    let out = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("running {program}"))?;
    if !out.status.success() {
        bail!(
            "{program} {} failed ({}): {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

fn uid() -> anyhow::Result<String> {
    let out = Command::new("id")
        .arg("-u")
        .output()
        .context("running id -u")?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn load(os: Os, path: &Path) -> anyhow::Result<()> {
    match os {
        Os::MacOs => {
            let target = format!("gui/{}", uid()?);
            let plist = path.display().to_string();
            if run("launchctl", &["bootstrap".into(), target, plist.clone()]).is_err() {
                run("launchctl", &["load".into(), "-w".into(), plist])?;
            }
        }
        Os::Linux => {
            run("systemctl", &["--user".into(), "daemon-reload".into()])?;
            run(
                "systemctl",
                &[
                    "--user".into(),
                    "enable".into(),
                    "--now".into(),
                    SYSTEMD_UNIT.into(),
                ],
            )?;
        }
    }
    Ok(())
}

fn unload(os: Os, path: &Path) -> anyhow::Result<()> {
    match os {
        Os::MacOs => {
            let target = format!("gui/{}/{LAUNCHD_LABEL}", uid()?);
            if run("launchctl", &["bootout".into(), target]).is_err() {
                run(
                    "launchctl",
                    &["unload".into(), "-w".into(), path.display().to_string()],
                )?;
            }
        }
        Os::Linux => {
            run(
                "systemctl",
                &[
                    "--user".into(),
                    "disable".into(),
                    "--now".into(),
                    SYSTEMD_UNIT.into(),
                ],
            )?;
        }
    }
    Ok(())
}

/// `owl install [--dry-run]`.
pub fn install(home: &Path, dry_run: bool) -> anyhow::Result<()> {
    let os = Os::current()?;
    let owl = owl_path()?;
    let home_abs =
        std::path::absolute(home).with_context(|| format!("resolving {}", home.display()))?;
    let text = unit_text(os, &owl, &home_abs);
    if dry_run {
        print!("{text}");
        return Ok(());
    }
    let path = unit_path(os, &user_home()?);
    write_unit(&path, &text)?;
    println!("installed {}", path.display());
    if no_load() {
        println!("load skipped ({NO_LOAD_ENV} is set)");
        return Ok(());
    }
    load(os, &path)?;
    println!("started {}", service_name(os));
    Ok(())
}

/// `owl uninstall`: stop + disable, then remove the unit. Idempotent when nothing is installed.
pub fn uninstall() -> anyhow::Result<()> {
    let os = Os::current()?;
    let path = unit_path(os, &user_home()?);
    if !path.exists() {
        println!("not installed ({} is absent)", path.display());
        return Ok(());
    }
    if no_load() {
        println!("stop skipped ({NO_LOAD_ENV} is set)");
    } else {
        unload(os, &path)?;
        println!("stopped {}", service_name(os));
    }
    std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    println!("removed {}", path.display());
    if os == Os::Linux && !no_load() {
        run("systemctl", &["--user".into(), "daemon-reload".into()])?;
    }
    Ok(())
}

fn service_name(os: Os) -> &'static str {
    match os {
        Os::MacOs => LAUNCHD_LABEL,
        Os::Linux => SYSTEMD_UNIT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_paths_per_os() {
        let h = Path::new("/Users/bea");
        assert_eq!(
            unit_path(Os::MacOs, h),
            PathBuf::from("/Users/bea/Library/LaunchAgents/dev.owlpost.owl.plist")
        );
        assert_eq!(
            unit_path(Os::Linux, h),
            PathBuf::from("/Users/bea/.config/systemd/user/owlpost.service")
        );
    }

    #[test]
    fn plist_runs_owl_daemon_with_home() {
        let text = unit_text(
            Os::MacOs,
            Path::new("/opt/bin/owl"),
            Path::new("/Users/bea/.config/owlpost"),
        );
        assert!(text.starts_with("<?xml"), "{text}");
        assert!(text.contains("<key>Label</key>\n    <string>dev.owlpost.owl</string>"));
        assert!(text.contains(
            "<string>/opt/bin/owl</string>\n        <string>daemon</string>\n        \
             <string>--home</string>\n        <string>/Users/bea/.config/owlpost</string>"
        ));
        assert!(text.contains("<key>RunAtLoad</key>\n    <true/>"));
        assert!(text.contains("<key>KeepAlive</key>\n    <true/>"));
        assert!(text.contains("<string>/Users/bea/.config/owlpost/daemon.log</string>"));
        // XML-sensitive characters are escaped.
        let odd = unit_text(Os::MacOs, Path::new("/a&b/owl"), Path::new("/h<1>"));
        assert!(odd.contains("<string>/a&amp;b/owl</string>"));
        assert!(odd.contains("<string>/h&lt;1&gt;</string>"));
    }

    #[test]
    fn systemd_unit_runs_owl_daemon_with_home() {
        let text = unit_text(
            Os::Linux,
            Path::new("/opt/bin/owl"),
            Path::new("/home/bea/.config/owlpost"),
        );
        assert!(text.starts_with("[Unit]\n"), "{text}");
        assert!(text.contains(
            "\nExecStart=\"/opt/bin/owl\" daemon --home \"/home/bea/.config/owlpost\"\n"
        ));
        assert!(text.contains("\nRestart=on-failure\n"));
        assert!(text.contains("\n[Install]\nWantedBy=default.target\n"));
        // Spaces and quotes survive systemd quoting.
        let odd = unit_text(Os::Linux, Path::new("/my bin/owl"), Path::new("/h\"q"));
        assert!(odd.contains("ExecStart=\"/my bin/owl\" daemon --home \"/h\\\"q\""));
    }

    #[test]
    fn os_current_matches_host() {
        let os = Os::current();
        if cfg!(target_os = "macos") {
            assert_eq!(os.unwrap(), Os::MacOs);
        } else if cfg!(target_os = "linux") {
            assert_eq!(os.unwrap(), Os::Linux);
        } else {
            assert!(os.is_err());
        }
    }

    #[test]
    fn owl_path_is_absolute_and_exists() {
        let p = owl_path().unwrap();
        assert!(p.is_absolute());
        assert!(p.is_file());
    }

    #[test]
    fn write_unit_is_atomic_and_creates_parents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("b").join("owlpost.service");
        write_unit(&path, "one").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one");
        assert!(!path.with_extension("tmp").exists());
        write_unit(&path, "two").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
    }

    #[test]
    fn write_unit_blocked_by_directory_is_a_clean_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("owlpost.service");
        std::fs::create_dir_all(path.join("child")).unwrap();
        let err = format!("{:#}", write_unit(&path, "x").unwrap_err());
        assert!(err.contains("renaming to"), "{err}");
        assert!(!path.with_extension("tmp").exists(), "temp file cleaned up");
        assert!(path.is_dir(), "blocker untouched");
        // Parent that is a file: create_dir_all fails.
        let file = dir.path().join("file");
        std::fs::write(&file, "").unwrap();
        let err = format!("{:#}", write_unit(&file.join("unit"), "x").unwrap_err());
        assert!(err.contains("creating"), "{err}");
    }
}
