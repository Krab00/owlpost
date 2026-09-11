//! Spool state machine (§8): `inbox/ outbox/ asks/ done/ cache/` under `$OWLPOST_HOME/spool`,
//! one JSON record per id, atomic writes (temp file + rename).

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Inbox,
    Outbox,
    Asks,
    Done,
    Cache,
}

impl Dir {
    pub const ALL: [Dir; 5] = [Dir::Inbox, Dir::Outbox, Dir::Asks, Dir::Done, Dir::Cache];

    pub fn name(self) -> &'static str {
        match self {
            Dir::Inbox => "inbox",
            Dir::Outbox => "outbox",
            Dir::Asks => "asks",
            Dir::Done => "done",
            Dir::Cache => "cache",
        }
    }
}

/// Stored shape from §6 plus `seen` and `meta`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub raw: String,
    pub sig: String,
    pub state: String,
    #[serde(default)]
    pub seen: bool,
    pub received_at: String,
    #[serde(default)]
    pub draft: Option<serde_json::Value>,
    #[serde(default)]
    pub meta: serde_json::Value,
}

pub struct Spool {
    root: PathBuf,
}

impl Spool {
    /// Creates `$home/spool/{inbox,outbox,asks,done,cache}`.
    pub fn new(home: &Path) -> anyhow::Result<Spool> {
        let root = home.join("spool");
        for d in Dir::ALL {
            let p = root.join(d.name());
            std::fs::create_dir_all(&p).with_context(|| format!("creating {}", p.display()))?;
        }
        Ok(Spool { root })
    }

    /// The `$OWLPOST_HOME` this spool lives under (`spool/`'s parent).
    pub fn home(&self) -> &Path {
        self.root.parent().unwrap_or(&self.root)
    }

    pub fn path(&self, dir: Dir, id: &str) -> PathBuf {
        self.root.join(dir.name()).join(format!("{id}.json"))
    }

    /// Writes `<id>.json.tmp` then renames over `<id>.json`.
    pub fn put(&self, dir: Dir, id: &str, rec: &Record) -> anyhow::Result<()> {
        let path = self.path(dir, id);
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(rec)?;
        std::fs::write(&tmp, json).with_context(|| format!("writing {}", tmp.display()))?;
        if let Err(e) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e).with_context(|| format!("renaming to {}", path.display()));
        }
        Ok(())
    }

    /// Missing → None. Malformed → error.
    pub fn get(&self, dir: Dir, id: &str) -> anyhow::Result<Option<Record>> {
        let path = self.path(dir, id);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .with_context(|| format!("parsing {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// All `*.json` records in `dir` passing `filter`, sorted by id.
    pub fn list(
        &self,
        dir: Dir,
        filter: impl Fn(&Record) -> bool,
    ) -> anyhow::Result<Vec<(String, Record)>> {
        let dir_path = self.root.join(dir.name());
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&dir_path)
            .with_context(|| format!("listing {}", dir_path.display()))?
        {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if let Some(rec) = self.get(dir, id)?
                && filter(&rec)
            {
                out.push((id.to_string(), rec));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    /// Like `list`, but a corrupt record is skipped with a warning instead of failing the
    /// whole listing (one bad file must not stall the outbox or the pull loop).
    pub fn list_lenient(&self, dir: Dir) -> anyhow::Result<Vec<(String, Record)>> {
        let dir_path = self.root.join(dir.name());
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&dir_path)
            .with_context(|| format!("listing {}", dir_path.display()))?
        {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            match self.get(dir, id) {
                Ok(Some(rec)) => out.push((id.to_string(), rec)),
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %format!("{e:#}"), "skipping corrupt record")
                }
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    fn update(&self, dir: Dir, id: &str, f: impl FnOnce(&mut Record)) -> anyhow::Result<()> {
        let mut rec = self
            .get(dir, id)?
            .with_context(|| format!("no record {id} in {}", dir.name()))?;
        f(&mut rec);
        self.put(dir, id, &rec)
    }

    pub fn set_state(&self, dir: Dir, id: &str, state: &str) -> anyhow::Result<()> {
        self.update(dir, id, |r| r.state = state.to_string())
    }

    pub fn mark_seen(&self, dir: Dir, id: &str) -> anyhow::Result<()> {
        self.update(dir, id, |r| r.seen = true)
    }

    /// Moves `<id>.json` between directories (same filesystem, so a rename).
    pub fn move_to(&self, from: Dir, id: &str, to: Dir) -> anyhow::Result<()> {
        let src = self.path(from, id);
        let dst = self.path(to, id);
        std::fs::rename(&src, &dst)
            .with_context(|| format!("moving {} to {}", src.display(), dst.display()))
    }

    pub fn count_unseen(&self, dir: Dir) -> anyhow::Result<usize> {
        Ok(self.list(dir, |r| !r.seen)?.len())
    }

    pub fn cache_get(&self, hash: &str) -> anyhow::Result<Option<Record>> {
        self.get(Dir::Cache, hash)
    }

    pub fn cache_put(&self, hash: &str, rec: &Record) -> anyhow::Result<()> {
        self.put(Dir::Cache, hash, rec)
    }
}
