//! `owl doctor` (§9): one line per check — `ok|warn|fail  <name>: <detail>` — for the key,
//! the config, every endpoint's host, the configured harness binaries, the daemon at
//! `daemon.addr` (card fetch, pinned to our own key) and the age of the last pull. Exit 1 when
//! any check fails; `--json` prints `[{check, status, detail}]`.

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::Context;
use serde::Serialize;
use serde_json::Value;

use crate::cli::ExitError;
use owlpost::config::Config;
use owlpost::daemon::ADDR_FILE;
use owlpost::identity::{self, Identity};
use owlpost::runner;
use owlpost::tls;

/// Written by the pull loop (OWL-008) after every successful pull; its mtime is the age source.
// ponytail: mtime, not content — works whatever OWL-008 writes into the file; parse a timestamp
// from it once the pull loop records one.
pub const LAST_PULL_FILE: &str = "last-pull";
/// How long the card fetch may take before the daemon counts as unreachable.
pub const DAEMON_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Fail => "fail",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub check: String,
    pub status: Status,
    pub detail: String,
}

impl Check {
    fn new(check: &str, status: Status, detail: impl Into<String>) -> Check {
        Check {
            check: check.into(),
            status,
            detail: detail.into(),
        }
    }
    fn ok(check: &str, detail: impl Into<String>) -> Check {
        Check::new(check, Status::Ok, detail)
    }
    fn warn(check: &str, detail: impl Into<String>) -> Check {
        Check::new(check, Status::Warn, detail)
    }
    fn fail(check: &str, detail: impl Into<String>) -> Check {
        Check::new(check, Status::Fail, detail)
    }

    /// `ok   key: owl:abc…` — status padded to four columns.
    pub fn line(&self) -> String {
        format!(
            "{:<4} {}: {}",
            self.status.as_str(),
            self.check,
            self.detail
        )
    }
}

/// `key`: the private key loads and has a fingerprint.
pub fn key_check(home: &Path) -> (Check, Option<Identity>) {
    match Identity::load(home) {
        Ok(id) => {
            let fp = identity::fingerprint(&id.verifying_key());
            (Check::ok("key", fp), Some(id))
        }
        Err(e) => (Check::fail("key", format!("{e:#}")), None),
    }
}

/// `config`: parses (defaults when absent). The returned config drives the later checks;
/// on failure it is the default so every remaining line still prints.
pub fn config_check(home: &Path) -> (Check, Config) {
    let path = Config::path(home);
    match Config::load(home) {
        Ok(cfg) if path.exists() => (Check::ok("config", path.display().to_string()), cfg),
        Ok(cfg) => (
            Check::warn(
                "config",
                format!("{} absent, using defaults", path.display()),
            ),
            cfg,
        ),
        Err(e) => (Check::fail("config", format!("{e:#}")), Config::default()),
    }
}

/// `endpoints`: one line per configured `host:port`, resolved with the system resolver;
/// a single `warn` when none is configured.
pub fn endpoint_checks(config: &Config) -> Vec<Check> {
    if config.endpoints.is_empty() {
        return vec![Check::warn(
            "endpoints",
            "none configured (peers cannot reach this daemon)",
        )];
    }
    config
        .endpoints
        .iter()
        .map(|ep| match ep.to_socket_addrs() {
            Ok(mut addrs) => match addrs.next() {
                Some(a) => Check::ok("endpoints", format!("{ep} -> {}", a.ip())),
                None => Check::fail("endpoints", format!("{ep}: resolves to nothing")),
            },
            Err(e) => Check::fail("endpoints", format!("{ep}: {e}")),
        })
        .collect()
}

/// `harness`: the responder's harness must resolve (`fail` while the responder is enabled,
/// `warn` otherwise); every other enabled harness that is missing is a `warn`.
pub fn harness_checks(
    config: &Config,
    path_var: Option<&std::ffi::OsStr>,
    user_home: &Path,
) -> Vec<Check> {
    let selected = &config.responder.harness;
    let miss = if config.responder.enabled {
        Status::Fail
    } else {
        Status::Warn
    };
    let mut out = Vec::new();
    if !config.harnesses.contains_key(selected) {
        out.push(Check::new(
            "harness",
            miss,
            format!("{selected}: selected in responder.harness but not configured"),
        ));
    }
    for (name, h) in &config.harnesses {
        let is_selected = name == selected;
        if !h.enabled {
            if is_selected {
                let why = h.disabled_reason.as_deref().unwrap_or("disabled");
                out.push(Check::new("harness", miss, format!("{name}: {why}")));
            }
            continue;
        }
        let Some(first) = h.cmd.first() else {
            out.push(Check::new(
                "harness",
                if is_selected { miss } else { Status::Warn },
                format!("{name}: empty cmd"),
            ));
            continue;
        };
        match runner::resolve_program_in(first, path_var, user_home) {
            Ok(p) => out.push(Check::ok("harness", format!("{name}: {}", p.display()))),
            Err(e) => out.push(Check::new(
                "harness",
                if is_selected { miss } else { Status::Warn },
                format!("{name}: {e:#}"),
            )),
        }
    }
    out
}

/// A wildcard bind address is not connectable; use the loopback of the same family.
pub fn connect_addr(addr: SocketAddr) -> SocketAddr {
    if addr.ip().is_unspecified() {
        let ip: IpAddr = match addr.ip() {
            IpAddr::V4(_) => "127.0.0.1".parse().expect("literal"),
            IpAddr::V6(_) => "::1".parse().expect("literal"),
        };
        SocketAddr::new(ip, addr.port())
    } else {
        addr
    }
}

/// `daemon`: `daemon.addr` exists, the card is served there and (when our key is known) the
/// card's fingerprint is ours.
pub fn daemon_check(home: &Path, id: Option<&Identity>) -> Check {
    let addr_path = home.join(ADDR_FILE);
    let raw = match std::fs::read_to_string(&addr_path) {
        Ok(s) => s,
        Err(_) => {
            return Check::fail(
                "daemon",
                format!("{} missing (is the daemon running?)", addr_path.display()),
            );
        }
    };
    let addr: SocketAddr = match raw.trim().parse() {
        Ok(a) => a,
        Err(_) => {
            return Check::fail(
                "daemon",
                format!(
                    "{} holds {:?}, not host:port",
                    addr_path.display(),
                    raw.trim()
                ),
            );
        }
    };
    let target = connect_addr(addr);
    match fetch_card(target, id) {
        Ok(card) => {
            let got = card
                .get("owlpost")
                .and_then(|o| o.get("fingerprint"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            match id.map(|i| identity::fingerprint(&i.verifying_key())) {
                Some(mine) if mine != got => Check::fail(
                    "daemon",
                    format!("reachable at {target} but serves fingerprint {got:?}, not ours"),
                ),
                _ => Check::ok("daemon", format!("reachable at {target} ({got})")),
            }
        }
        Err(e) => Check::fail("daemon", format!("{target}: {e:#}")),
    }
}

fn fetch_card(target: SocketAddr, id: Option<&Identity>) -> anyhow::Result<Value> {
    let expected = id.map(|i| *i.verifying_key().as_bytes());
    let tls_cfg = tls::client_config(None, expected)?;
    let client = reqwest::Client::builder()
        .use_preconfigured_tls(Arc::unwrap_or_clone(tls_cfg))
        .timeout(DAEMON_TIMEOUT)
        .build()
        .context("building HTTP client")?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting runtime")?;
    rt.block_on(async move {
        let resp = client
            .get(format!("https://{target}/.well-known/agent-card.json"))
            .send()
            .await
            .context("connecting")?;
        if !resp.status().is_success() {
            anyhow::bail!("card request returned {}", resp.status());
        }
        resp.json::<Value>().await.context("card is not JSON")
    })
}

fn human_age(age: Duration) -> String {
    let s = age.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86_400)
    }
}

/// `pull`: age of `last-pull` (mtime). Absent → `warn`; older than twice the pull interval →
/// `warn` (the pull loop is not keeping up); else `ok`.
pub fn pull_check(home: &Path, config: &Config, now: SystemTime) -> Check {
    let path = home.join(LAST_PULL_FILE);
    let modified = match std::fs::metadata(&path).and_then(|m| m.modified()) {
        Ok(m) => m,
        Err(_) => return Check::warn("pull", "never pulled (no last-pull file)"),
    };
    let age = now.duration_since(modified).unwrap_or(Duration::ZERO);
    let detail = format!("last pull {} ago", human_age(age));
    if age.as_secs() > config.pull_interval_secs.saturating_mul(2) {
        Check::warn(
            "pull",
            format!("{detail} (interval {}s)", config.pull_interval_secs),
        )
    } else {
        Check::ok("pull", detail)
    }
}

/// Every check in report order.
pub fn run_checks(home: &Path) -> Vec<Check> {
    let (key, id) = key_check(home);
    let (config_line, config) = config_check(home);
    let mut out = vec![key, config_line];
    out.extend(endpoint_checks(&config));
    let user_home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    out.extend(harness_checks(
        &config,
        std::env::var_os("PATH").as_deref(),
        &user_home,
    ));
    out.push(daemon_check(home, id.as_ref()));
    out.push(pull_check(home, &config, SystemTime::now()));
    out
}

pub fn render(checks: &[Check], json: bool) -> String {
    if json {
        format!(
            "{}\n",
            serde_json::to_string_pretty(checks).expect("checks serialise")
        )
    } else {
        checks.iter().map(|c| c.line() + "\n").collect()
    }
}

/// `owl doctor [--json]`.
pub fn run(home: &Path, json: bool) -> anyhow::Result<()> {
    let checks = run_checks(home);
    print!("{}", render(&checks, json));
    let failed = checks.iter().filter(|c| c.status == Status::Fail).count();
    if failed > 0 {
        return Err(ExitError::new(1, format!("{failed} check(s) failed")).into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use owlpost::config::Harness;

    fn statuses(checks: &[Check]) -> Vec<(String, Status)> {
        checks.iter().map(|c| (c.check.clone(), c.status)).collect()
    }

    #[test]
    fn line_and_json_shapes() {
        let c = Check::ok("key", "owl:abc");
        assert_eq!(c.line(), "ok   key: owl:abc");
        assert_eq!(Check::warn("pull", "x").line(), "warn pull: x");
        assert_eq!(Check::fail("daemon", "y").line(), "fail daemon: y");
        let text = render(&[c.clone(), Check::fail("daemon", "y")], false);
        assert_eq!(text, "ok   key: owl:abc\nfail daemon: y\n");
        let json: Value = serde_json::from_str(&render(&[c], true)).unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].get("check").and_then(Value::as_str), Some("key"));
        assert_eq!(arr[0].get("status").and_then(Value::as_str), Some("ok"));
        assert_eq!(
            arr[0].get("detail").and_then(Value::as_str),
            Some("owl:abc")
        );
    }

    #[test]
    fn key_and_config_checks() {
        let home = tempfile::tempdir().unwrap();
        let (c, id) = key_check(home.path());
        assert_eq!(c.status, Status::Fail);
        assert!(id.is_none());
        let (c, cfg) = config_check(home.path());
        assert_eq!(c.status, Status::Warn, "absent config is a warning: {c:?}");
        assert_eq!(cfg, Config::default());
        Identity::from_seed([1u8; 32]).save(home.path()).unwrap();
        Config {
            name: "Bea".into(),
            ..Default::default()
        }
        .save(home.path())
        .unwrap();
        let (c, id) = key_check(home.path());
        assert_eq!(c.status, Status::Ok);
        assert_eq!(
            c.detail,
            identity::fingerprint(&id.unwrap().verifying_key())
        );
        let (c, cfg) = config_check(home.path());
        assert_eq!(c.status, Status::Ok);
        assert_eq!(cfg.name, "Bea");
        std::fs::write(Config::path(home.path()), "{broken").unwrap();
        let (c, cfg) = config_check(home.path());
        assert_eq!(c.status, Status::Fail);
        assert!(c.detail.contains("parsing"), "{c:?}");
        assert_eq!(
            cfg,
            Config::default(),
            "defaults keep the later checks running"
        );
    }

    #[test]
    fn endpoint_checks_resolve_or_fail() {
        let mut cfg = Config::default();
        let none = endpoint_checks(&cfg);
        assert_eq!(statuses(&none), [("endpoints".to_string(), Status::Warn)]);
        cfg.endpoints = vec![
            "127.0.0.1:7411".into(),
            "localhost:7411".into(),
            "noport".into(),
        ];
        let checks = endpoint_checks(&cfg);
        assert_eq!(
            statuses(&checks),
            [
                ("endpoints".to_string(), Status::Ok),
                ("endpoints".to_string(), Status::Ok),
                ("endpoints".to_string(), Status::Fail),
            ]
        );
        assert_eq!(checks[0].detail, "127.0.0.1:7411 -> 127.0.0.1");
        assert!(checks[2].detail.starts_with("noport: "), "{:?}", checks[2]);
    }

    #[test]
    fn harness_checks_cover_selected_disabled_and_kimi_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("claude"), "").unwrap();
        let path_var = std::env::join_paths([&bin]).unwrap();
        let user_home = dir.path().join("home");
        let mut cfg = Config::default();
        cfg.harnesses
            .retain(|k, _| k == "claude" || k == "codex" || k == "kimi");
        // claude (selected) on PATH: ok; codex missing: warn; kimi disabled + unselected: skipped.
        let checks = harness_checks(&cfg, Some(&path_var), &user_home);
        assert_eq!(
            statuses(&checks),
            [
                ("harness".to_string(), Status::Ok),
                ("harness".to_string(), Status::Warn),
            ]
        );
        assert_eq!(checks[0].detail, "claude: claude");
        assert!(checks[1].detail.starts_with("codex: "), "{:?}", checks[1]);
        // Selected harness missing from PATH → fail; with the responder off → warn.
        cfg.responder.harness = "codex".into();
        let checks = harness_checks(&cfg, Some(&path_var), &user_home);
        assert_eq!(
            checks
                .iter()
                .find(|c| c.detail.starts_with("codex"))
                .unwrap()
                .status,
            Status::Fail
        );
        cfg.responder.enabled = false;
        let checks = harness_checks(&cfg, Some(&path_var), &user_home);
        assert_eq!(
            checks
                .iter()
                .find(|c| c.detail.starts_with("codex"))
                .unwrap()
                .status,
            Status::Warn
        );
        cfg.responder.enabled = true;
        // Selected harness not configured at all → fail.
        cfg.responder.harness = "ghost".into();
        let checks = harness_checks(&cfg, Some(&path_var), &user_home);
        assert_eq!(checks[0].status, Status::Fail);
        assert!(checks[0].detail.contains("ghost"), "{:?}", checks[0]);
        // Selected but disabled → fail with the reason.
        cfg.responder.harness = "kimi".into();
        let checks = harness_checks(&cfg, Some(&path_var), &user_home);
        let kimi = checks
            .iter()
            .find(|c| c.detail.starts_with("kimi"))
            .unwrap();
        assert_eq!(kimi.status, Status::Fail);
        assert!(kimi.detail.contains("read-only"), "{kimi:?}");
        // kimi enabled, not on PATH, found under ~/.kimi-code/bin.
        cfg.harnesses.get_mut("kimi").unwrap().enabled = true;
        let checks = harness_checks(&cfg, Some(&path_var), &user_home);
        let kimi = checks
            .iter()
            .find(|c| c.detail.starts_with("kimi"))
            .unwrap();
        assert_eq!(kimi.status, Status::Fail, "{kimi:?}");
        let fallback = user_home.join(".kimi-code").join("bin");
        std::fs::create_dir_all(&fallback).unwrap();
        std::fs::write(fallback.join("kimi"), "").unwrap();
        let checks = harness_checks(&cfg, Some(&path_var), &user_home);
        let kimi = checks
            .iter()
            .find(|c| c.detail.starts_with("kimi"))
            .unwrap();
        assert_eq!(kimi.status, Status::Ok, "{kimi:?}");
        assert_eq!(
            kimi.detail,
            format!("kimi: {}", fallback.join("kimi").display())
        );
        // Empty cmd is reported, not panicked on.
        cfg.harnesses.insert(
            "empty".into(),
            Harness {
                cmd: vec![],
                answer_path: "raw".into(),
                enabled: true,
                disabled_reason: None,
                env: Default::default(),
            },
        );
        let checks = harness_checks(&cfg, Some(&path_var), &user_home);
        let empty = checks
            .iter()
            .find(|c| c.detail.starts_with("empty"))
            .unwrap();
        assert_eq!(
            (empty.status, empty.detail.as_str()),
            (Status::Warn, "empty: empty cmd")
        );
    }

    #[test]
    fn connect_addr_maps_wildcards_to_loopback() {
        let v4: SocketAddr = "0.0.0.0:7411".parse().unwrap();
        assert_eq!(connect_addr(v4).to_string(), "127.0.0.1:7411");
        let v6: SocketAddr = "[::]:7411".parse().unwrap();
        assert_eq!(connect_addr(v6).to_string(), "[::1]:7411");
        let fixed: SocketAddr = "10.0.0.5:1".parse().unwrap();
        assert_eq!(connect_addr(fixed), fixed);
    }

    #[test]
    fn daemon_check_without_addr_file_or_listener_fails() {
        let home = tempfile::tempdir().unwrap();
        let c = daemon_check(home.path(), None);
        assert_eq!(c.status, Status::Fail);
        assert!(c.detail.contains("daemon.addr missing"), "{c:?}");
        std::fs::write(home.path().join(ADDR_FILE), "garbage\n").unwrap();
        let c = daemon_check(home.path(), None);
        assert_eq!(c.status, Status::Fail);
        assert!(c.detail.contains("not host:port"), "{c:?}");
        // A port nobody listens on: connection refused → fail naming the address.
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = free.local_addr().unwrap();
        drop(free);
        std::fs::write(home.path().join(ADDR_FILE), format!("{addr}\n")).unwrap();
        let c = daemon_check(home.path(), None);
        assert_eq!(c.status, Status::Fail);
        assert!(c.detail.starts_with(&addr.to_string()), "{c:?}");
    }

    #[test]
    fn pull_check_ages() {
        let home = tempfile::tempdir().unwrap();
        let cfg = Config::default();
        let now = SystemTime::now();
        let c = pull_check(home.path(), &cfg, now);
        assert_eq!(
            (c.status, c.detail.as_str()),
            (Status::Warn, "never pulled (no last-pull file)")
        );
        std::fs::write(home.path().join(LAST_PULL_FILE), "").unwrap();
        // Age is measured from the file's mtime, so anchor "now" there.
        let now = std::fs::metadata(home.path().join(LAST_PULL_FILE))
            .unwrap()
            .modified()
            .unwrap();
        let c = pull_check(home.path(), &cfg, now + Duration::from_secs(5));
        assert_eq!(c.status, Status::Ok);
        assert!(c.detail.starts_with("last pull 5s ago"), "{c:?}");
        // Older than 2 × pull_interval_secs (60 s default): warn.
        let c = pull_check(home.path(), &cfg, now + Duration::from_secs(121));
        assert_eq!(c.status, Status::Warn);
        assert!(
            c.detail.contains("last pull 2m ago (interval 60s)"),
            "{c:?}"
        );
        let c = pull_check(home.path(), &cfg, now + Duration::from_secs(120));
        assert_eq!(c.status, Status::Ok, "{c:?}");
        // A file from the future is age 0, never negative.
        let c = pull_check(home.path(), &cfg, now - Duration::from_secs(3600));
        assert_eq!(
            (c.status, c.detail.as_str()),
            (Status::Ok, "last pull 0s ago")
        );
        assert_eq!(human_age(Duration::from_secs(7200)), "2h");
        assert_eq!(human_age(Duration::from_secs(3 * 86_400)), "3d");
    }
}
