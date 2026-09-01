//! `Config` struct, `$OWLPOST_HOME` resolution, load/save with §3 defaults.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

pub const DEFAULT_LISTEN: &str = "0.0.0.0:7411";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub name: String,
    pub emails: Vec<String>,
    pub listen: String,
    pub endpoints: Vec<String>,
    pub pull_interval_secs: u64,
    pub outbox_ttl_days: u64,
    pub rate_limit_per_peer_per_hour: u64,
    pub notify: bool,
    pub responder: Responder,
    pub harnesses: BTreeMap<String, Harness>,
    pub projects: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Responder {
    pub enabled: bool,
    pub harness: String,
    pub scope: Scope,
    pub redact: Vec<String>,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Scope {
    pub repo: bool,
    pub project_files: bool,
    pub private_memory: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Harness {
    pub cmd: Vec<String>,
    pub answer_path: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

fn yes() -> bool {
    true
}

impl Default for Scope {
    fn default() -> Self {
        Self {
            repo: true,
            project_files: true,
            private_memory: false,
        }
    }
}

impl Default for Responder {
    fn default() -> Self {
        Self {
            enabled: true,
            harness: "claude".into(),
            scope: Scope::default(),
            redact: vec![
                r"(?i)(api[_-]?key|secret|token|password)\s*[:=]\s*\S+".into(),
                r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----"
                    .into(),
            ],
            timeout_secs: 180,
        }
    }
}

fn harness(cmd: &[&str], answer_path: &str) -> Harness {
    Harness {
        cmd: cmd.iter().map(|s| s.to_string()).collect(),
        answer_path: answer_path.into(),
        enabled: true,
        disabled_reason: None,
        env: BTreeMap::new(),
    }
}

impl Default for Config {
    fn default() -> Self {
        let mut harnesses = BTreeMap::new();
        harnesses.insert(
            "claude".into(),
            harness(
                &[
                    "claude",
                    "-p",
                    "--allowed-tools",
                    "Read,Grep,Glob",
                    "--output-format",
                    "json",
                    "{prompt}",
                ],
                "result",
            ),
        );
        harnesses.insert(
            "codex".into(),
            harness(
                &[
                    "codex",
                    "exec",
                    "--sandbox",
                    "read-only",
                    "--ephemeral",
                    "--json",
                    "{prompt}",
                ],
                "last_message",
            ),
        );
        harnesses.insert(
            "opencode".into(),
            harness(
                &[
                    "opencode",
                    "run",
                    "--format",
                    "json",
                    "--agent",
                    "owl-readonly",
                    "{prompt}",
                ],
                "last_text",
            ),
        );
        harnesses.insert(
            "kimi".into(),
            Harness {
                enabled: false,
                disabled_reason: Some(
                    "read-only enforcement under -p unverified (see concept.md open questions)"
                        .into(),
                ),
                ..harness(
                    &["kimi", "-p", "{prompt}", "--output-format", "stream-json"],
                    "last_text",
                )
            },
        );
        harnesses.insert(
            "fake".into(),
            harness(&["tests/fixtures/fake-harness.sh", "{prompt}"], "raw"),
        );
        Self {
            name: String::new(),
            emails: Vec::new(),
            listen: DEFAULT_LISTEN.into(),
            endpoints: Vec::new(),
            pull_interval_secs: 60,
            outbox_ttl_days: 14,
            rate_limit_per_peer_per_hour: 20,
            notify: true,
            responder: Responder::default(),
            harnesses,
            projects: BTreeMap::new(),
        }
    }
}

/// `--home` > `$OWLPOST_HOME` > `$HOME/.config/owlpost`.
pub fn home_dir(cli_home: Option<&Path>) -> PathBuf {
    if let Some(h) = cli_home {
        return h.to_path_buf();
    }
    if let Some(h) = std::env::var_os("OWLPOST_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(h);
    }
    // ponytail: HOME only — no `dirs` crate; Windows is not a target.
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
        .join(".config")
        .join("owlpost")
}

impl Config {
    pub fn path(home: &Path) -> PathBuf {
        home.join("config.json")
    }

    /// Missing file → defaults. Malformed file → error.
    pub fn load(home: &Path) -> anyhow::Result<Config> {
        let path = Self::path(home);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    // ponytail: dead_code allow until `owl init` (OWL-002) calls this from the bin.
    #[allow(dead_code)]
    pub fn save(&self, home: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(home).with_context(|| format!("creating {}", home.display()))?;
        let path = Self::path(home);
        let json = serde_json::to_vec_pretty(self)?;
        std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_missing() {
        let home = tempfile::tempdir().unwrap();
        let cfg = Config::load(home.path()).unwrap();
        assert_eq!(cfg.listen, "0.0.0.0:7411");
        assert_eq!(cfg.pull_interval_secs, 60);
        assert_eq!(cfg.rate_limit_per_peer_per_hour, 20);
        for key in ["claude", "codex", "opencode", "kimi", "fake"] {
            assert!(cfg.harnesses.contains_key(key), "missing harness {key}");
        }
        assert!(!cfg.harnesses["kimi"].enabled);
        assert!(cfg.harnesses["kimi"].disabled_reason.is_some());
        assert!(cfg.harnesses["claude"].enabled);
        assert_eq!(cfg.outbox_ttl_days, 14);
        assert!(cfg.notify);
        assert_eq!(cfg.responder.harness, "claude");
        assert_eq!(cfg.responder.timeout_secs, 180);
        assert_eq!(cfg.responder.redact.len(), 2);
        assert!(cfg.name.is_empty() && cfg.emails.is_empty());
        assert!(cfg.endpoints.is_empty() && cfg.projects.is_empty());
    }

    #[test]
    fn roundtrip() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path().join("nested"); // save must create the dir
        let mut cfg = Config {
            name: "Krzysiek".into(),
            emails: vec!["k@example.com".into()],
            endpoints: vec!["host:7411".into()],
            ..Default::default()
        };
        cfg.projects
            .insert("github.com/x/y".into(), "/tmp/y".into());
        cfg.harnesses
            .get_mut("fake")
            .unwrap()
            .env
            .insert("FOO".into(), "bar".into());
        cfg.save(&home).unwrap();
        let loaded = Config::load(&home).unwrap();
        assert_eq!(loaded, cfg);
    }

    #[test]
    fn partial_file_fills_defaults() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            Config::path(home.path()),
            r#"{"name":"A","harnesses":{"x":{"cmd":["x"],"answer_path":"raw"}}}"#,
        )
        .unwrap();
        let cfg = Config::load(home.path()).unwrap();
        assert_eq!(cfg.name, "A");
        assert_eq!(cfg.listen, DEFAULT_LISTEN);
        assert!(cfg.harnesses["x"].enabled);
        assert!(cfg.harnesses["x"].env.is_empty());
    }

    #[test]
    fn malformed_file_is_error() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(Config::path(home.path()), "{not json").unwrap();
        assert!(Config::load(home.path()).is_err());
    }

    #[test]
    fn home_precedence() {
        // Env-var mutation is process-global; keep it inside one test.
        let cli = tempfile::tempdir().unwrap();
        let env_home = tempfile::tempdir().unwrap();
        let user_home = tempfile::tempdir().unwrap();
        // SAFETY: tests in this module run in one process; no other test touches these vars.
        unsafe {
            std::env::set_var("HOME", user_home.path());
            std::env::set_var("OWLPOST_HOME", env_home.path());
        }
        assert_eq!(home_dir(Some(cli.path())), cli.path());
        assert_eq!(home_dir(None), env_home.path());
        unsafe {
            std::env::remove_var("OWLPOST_HOME");
        }
        assert_eq!(
            home_dir(None),
            user_home.path().join(".config").join("owlpost")
        );
    }
}
