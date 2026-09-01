//! Local provider: `$OWLPOST_HOME/contacts/*.json` (full contacts or policy overlays).

use std::path::{Path, PathBuf};

use super::{Contact, load_dir};

pub fn dir(home: &Path) -> PathBuf {
    home.join("contacts")
}

pub fn load(home: &Path) -> Vec<Contact> {
    load_dir(&dir(home), "local")
}
