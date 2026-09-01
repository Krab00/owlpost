//! Repo provider: `<git root>/.agents/peers/*.json`.

use std::path::{Path, PathBuf};

use super::{Contact, load_dir};

/// Walk up from `cwd` to the first directory containing `.git` (dir or worktree file).
pub fn find_git_root(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .find(|d| d.join(".git").exists())
        .map(Path::to_path_buf)
}

pub fn load(root: &Path) -> Vec<Contact> {
    load_dir(&root.join(".agents").join("peers"), "repo")
}
