//! `owl deny <peer>` (§5, §8, §9): policy `never` in the local overlay, then every held
//! `consent` record from that peer is finished into `done/` with state `denied` (done copy
//! first, inbox unlink after — the shared `finish`). From now on the daemon answers that peer
//! `403 unavailable`, indistinguishable from being offline.

use std::path::Path;

use owlpost::contacts::{Mode, Policy, Scope};
use owlpost::spool::{Dir, Spool};
use serde_json::json;

use super::{contact_book, finish, payload_of, print_json, user_error};

/// Finishes every `consent` record from `fingerprint` as `denied`; returns their ids.
pub fn deny_held(spool: &Spool, fingerprint: &str) -> anyhow::Result<Vec<String>> {
    let mut denied = Vec::new();
    for (id, rec) in spool.list(Dir::Inbox, |r| r.state == "consent")? {
        if payload_of(&id, &rec)?.from != fingerprint {
            continue;
        }
        finish(
            spool,
            &id,
            rec,
            "denied",
            &[("previous_state", json!("consent"))],
        )?;
        denied.push(id);
    }
    Ok(denied)
}

pub fn run(home: &Path, peer: &str, json: bool) -> anyhow::Result<()> {
    let mut book = contact_book(home)?;
    let contact = book.resolve(peer).map_err(|e| user_error(e.to_string()))?;
    let fingerprint = contact.fingerprint.clone();
    let name = contact.name.clone();
    book.set_policy(
        home,
        &fingerprint,
        Policy {
            mode: Mode::Never,
            scope: Scope::default(),
            rate_limit_per_hour: None,
        },
    )?;
    let spool = Spool::new(home)?;
    let denied = deny_held(&spool, &fingerprint)?;
    if json {
        print_json(&json!({
            "peer": name,
            "fingerprint": fingerprint,
            "policy": "never",
            "denied": denied,
        }))?;
    } else {
        println!(
            "denied {name} ({fingerprint}): policy never, {} held question{} moved to done",
            denied.len(),
            if denied.len() == 1 { "" } else { "s" }
        );
    }
    Ok(())
}
