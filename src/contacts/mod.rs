//! Contact book: the two scopes merged, policy overlay, lookup. See docs/technical-design.md §5.
//!
//! Scope names follow the user's view: **global** = `$OWLPOST_HOME/contacts/` (this machine,
//! every repo; module [`local`]), **local** = `<git root>/.agents/peers/` (this repository,
//! shared via PR; module [`repo`]).

pub mod local;
pub mod repo;

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

use crate::identity;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    /// May be empty in a policy overlay file (only `pubkey` + `policy` matter there).
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub emails: Vec<String>,
    pub pubkey: String,
    #[serde(default)]
    pub endpoints: Vec<String>,
    /// `"global"` (`$OWLPOST_HOME/contacts/`) or `"local"` (`.agents/peers/`); set by the
    /// provider, not trusted from the file.
    #[serde(default)]
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<Policy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_at: Option<String>,
    /// Derived from `pubkey` at load time; never read from the file.
    #[serde(skip_deserializing)]
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    pub mode: Mode,
    #[serde(default)]
    pub scope: Scope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_per_hour: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Manual,
    Auto,
    Never,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Manual => "manual",
            Mode::Auto => "auto",
            Mode::Never => "never",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub projects: Vec<String>,
}

impl Default for Scope {
    fn default() -> Self {
        Scope {
            projects: vec!["*".into()],
        }
    }
}

impl Contact {
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

/// Parse one contact file; a bad pubkey counts as malformed.
fn parse(path: &Path, source: &str) -> anyhow::Result<Contact> {
    let bytes = std::fs::read(path)?;
    let mut c: Contact = serde_json::from_slice(&bytes)?;
    let pk = identity::parse_pubkey(&c.pubkey)?;
    c.fingerprint = identity::fingerprint(&pk);
    c.source = source.to_string();
    Ok(c)
}

/// Load every `*.json` in `dir`; malformed files are skipped with a warning on stderr.
fn load_dir(dir: &Path, source: &str) -> Vec<Contact> {
    load_dir_paths(dir, source).into_iter().map(|(_, c)| c).collect()
}

/// [`load_dir`] keeping each contact's file path (for `owl contact remove`).
fn load_dir_paths(dir: &Path, source: &str) -> Vec<(PathBuf, Contact)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    let mut out = Vec::new();
    for p in paths {
        match parse(&p, source) {
            Ok(c) => out.push((p, c)),
            Err(e) => eprintln!("owl: warning: skipping {}: {e:#}", p.display()),
        }
    }
    out
}

#[derive(Debug, Default, Clone)]
pub struct ContactBook {
    pub contacts: Vec<Contact>,
}

impl ContactBook {
    /// Local (repo) contacts first (from the git root above `cwd`), then global ones. A global
    /// file whose pubkey matches a local contact contributes only its `policy`.
    pub fn load(home: &Path, cwd: &Path) -> anyhow::Result<ContactBook> {
        let mut contacts = repo::find_git_root(cwd)
            .map(|r| repo::load(&r))
            .unwrap_or_default();
        for l in local::load(home) {
            match contacts.iter_mut().find(|c| c.pubkey == l.pubkey) {
                Some(existing) => existing.policy = l.policy,
                None => contacts.push(l),
            }
        }
        Ok(ContactBook { contacts })
    }

    /// The merged book restricted to contacts whose `source` is `scope` (`"global"` or
    /// `"local"`); a policy overlay stays with the contact it belongs to.
    pub fn load_scope(home: &Path, cwd: &Path, scope: &str) -> anyhow::Result<ContactBook> {
        let mut book = ContactBook::load(home, cwd)?;
        book.contacts.retain(|c| c.source == scope);
        Ok(book)
    }

    /// The files of one scope with the contact each holds: `global` = `$home/contacts/`
    /// minus the policy overlays of local contacts; `local` = `.agents/peers/` of the git
    /// root above `cwd` (`Err` when there is none).
    pub fn scope_files(
        home: &Path,
        cwd: &Path,
        scope: &str,
    ) -> anyhow::Result<Vec<(PathBuf, Contact)>> {
        if scope == "local" {
            let root = repo::find_git_root(cwd)
                .with_context(|| format!("{} is not inside a git repository", cwd.display()))?;
            return Ok(load_dir_paths(&repo::peers_dir(&root), "local"));
        }
        let local: Vec<Contact> = repo::find_git_root(cwd)
            .map(|r| repo::load(&r))
            .unwrap_or_default();
        let mut files = load_dir_paths(&local::dir(home), "global");
        files.retain(|(_, c)| !local.iter().any(|l| l.pubkey == c.pubkey));
        Ok(files)
    }

    /// Exact fingerprint → exact email → unique case-insensitive name prefix.
    pub fn resolve(&self, query: &str) -> anyhow::Result<&Contact> {
        if let Some(c) = self.contacts.iter().find(|c| c.fingerprint == query) {
            return Ok(c);
        }
        if let Some(c) = self
            .contacts
            .iter()
            .find(|c| c.emails.iter().any(|e| e == query))
        {
            return Ok(c);
        }
        let q = query.to_lowercase();
        let hits: Vec<&Contact> = self
            .contacts
            .iter()
            .filter(|c| c.name.to_lowercase().starts_with(&q))
            .collect();
        match hits.as_slice() {
            [one] => Ok(one),
            [] => bail!("no contact matches {query:?}"),
            many => {
                let names: Vec<&str> = many.iter().map(|c| c.name.as_str()).collect();
                bail!("ambiguous peer {query:?}: matches {}", names.join(", "))
            }
        }
    }

    pub fn policy_for(&self, fingerprint: &str) -> Option<&Policy> {
        self.contacts
            .iter()
            .find(|c| c.fingerprint == fingerprint)
            .and_then(|c| c.policy.as_ref())
    }

    /// Write/update the overlay `$home/contacts/<fingerprint>.json`. For a local (repo) contact
    /// the file holds only pubkey + policy (+ source/added_at); an existing file keeps its fields.
    pub fn set_policy(
        &mut self,
        home: &Path,
        fingerprint: &str,
        policy: Policy,
    ) -> anyhow::Result<()> {
        let contact = self
            .contacts
            .iter_mut()
            .find(|c| c.fingerprint == fingerprint)
            .with_context(|| format!("no contact with fingerprint {fingerprint}"))?;
        let dir = local::dir(home);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{fingerprint}.json"));
        let mut v: serde_json::Value = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing {}", path.display()))?,
            Err(_) if contact.source == "local" => serde_json::json!({
                "pubkey": contact.pubkey,
                "source": "global",
                "added_at": rfc3339(now_secs()),
            }),
            Err(_) => {
                let mut full = contact.clone();
                full.added_at.get_or_insert_with(|| rfc3339(now_secs()));
                serde_json::to_value(&full)?
            }
        };
        let obj = v
            .as_object_mut()
            .with_context(|| format!("{} is not a JSON object", path.display()))?;
        obj.insert("policy".into(), serde_json::to_value(&policy)?);
        obj.remove("fingerprint");
        std::fs::write(&path, serde_json::to_vec_pretty(&v)?)?;
        contact.policy = Some(policy);
        Ok(())
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ponytail: hand-rolled UTC formatting (Hinnant's civil_from_days) instead of a chrono dep.
fn rfc3339(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_known_instants() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_788_256_800), "2026-09-01T10:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_735_689_599), "2024-12-31T23:59:59Z");
    }

    #[test]
    fn mode_roundtrip() {
        for (m, s) in [
            (Mode::Manual, "\"manual\""),
            (Mode::Auto, "\"auto\""),
            (Mode::Never, "\"never\""),
        ] {
            assert_eq!(serde_json::to_string(&m).unwrap(), s);
            assert_eq!(serde_json::from_str::<Mode>(s).unwrap(), m);
        }
        assert!(serde_json::from_str::<Mode>("\"Auto\"").is_err());
    }

    #[test]
    fn scope_defaults_to_star() {
        let p: Policy = serde_json::from_str(r#"{"mode":"auto"}"#).unwrap();
        assert_eq!(p.scope.projects, ["*"]);
        assert_eq!(p.rate_limit_per_hour, None);
    }
}
