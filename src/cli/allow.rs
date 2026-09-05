//! `owl allow <peer> [--once | --always] [--i-verified-the-fingerprint]` (§5, §8, §9,
//! concept "Trust").
//!
//! * `--once`: release every held `consent` record from that peer to `pending`; no policy
//!   is written, so the next question is held again.
//! * default: write policy `manual` to the local overlay, then release.
//! * `--always`: write policy `auto`, then release. A `global`-source contact (TOFU, key never
//!   verified out of band) needs `--i-verified-the-fingerprint`; without it nothing is written
//!   and nothing is released (exit 1). Repo contacts were merged through a reviewed PR and
//!   need no flag.

use std::path::Path;

use owlpost::contacts::{Contact, Mode, Policy, Scope};
use owlpost::spool::{Dir, Spool};
use serde_json::json;

use super::{contact_book, payload_of, print_json, user_error};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opts {
    pub once: bool,
    pub always: bool,
    pub verified: bool,
}

/// The policy `owl allow` writes for `opts`, or `None` for `--once`; the flag conflict and the
/// global-contact guard are user errors that must leave the home untouched.
pub fn policy_for(contact: &Contact, opts: Opts) -> anyhow::Result<Option<Mode>> {
    if opts.once && opts.always {
        return Err(user_error(
            "--once and --always are mutually exclusive: release the held question only (--once) or answer automatically from now on (--always)",
        ));
    }
    if opts.once {
        return Ok(None);
    }
    if !opts.always {
        return Ok(Some(Mode::Manual));
    }
    if contact.source == "global" && !opts.verified {
        return Err(user_error(format!(
            "{} ({}) was added by hand, so its key was never verified through a reviewed repo PR; \
             auto-accept for it needs the fingerprint compared out of band — repeat with \
             --always --i-verified-the-fingerprint once you have done that",
            contact.name, contact.fingerprint
        )));
    }
    Ok(Some(Mode::Auto))
}

/// Moves every `consent` record from `fingerprint` to `pending`; returns their ids.
pub fn release(spool: &Spool, fingerprint: &str) -> anyhow::Result<Vec<String>> {
    let mut released = Vec::new();
    for (id, rec) in spool.list(Dir::Inbox, |r| r.state == "consent")? {
        if payload_of(&id, &rec)?.from != fingerprint {
            continue;
        }
        spool.set_state(Dir::Inbox, &id, "pending")?;
        released.push(id);
    }
    Ok(released)
}

pub fn run(home: &Path, peer: &str, opts: Opts, json: bool) -> anyhow::Result<()> {
    let mut book = contact_book(home)?;
    let contact = book.resolve(peer).map_err(|e| user_error(e.to_string()))?;
    let fingerprint = contact.fingerprint.clone();
    let name = contact.name.clone();
    let mode = policy_for(contact, opts)?;
    if let Some(mode) = mode {
        book.set_policy(
            home,
            &fingerprint,
            Policy {
                mode,
                scope: Scope::default(),
                rate_limit_per_hour: None,
            },
        )?;
    }
    let spool = Spool::new(home)?;
    let released = release(&spool, &fingerprint)?;
    let policy = mode.map(Mode::as_str);
    if json {
        print_json(&json!({
            "peer": name,
            "fingerprint": fingerprint,
            "policy": policy,
            "released": released,
        }))?;
    } else {
        let what = match policy {
            Some(p) => format!("policy {p}"),
            None => "no policy written (once)".to_string(),
        };
        println!(
            "allowed {name} ({fingerprint}): {what}, released {} held question{}",
            released.len(),
            if released.len() == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contact(source: &str) -> Contact {
        Contact {
            name: "Ana".into(),
            emails: vec![],
            pubkey: "ed25519:x".into(),
            endpoints: vec![],
            source: source.into(),
            policy: None,
            added_at: None,
            fingerprint: "FPFP".into(),
        }
    }

    fn opts(once: bool, always: bool, verified: bool) -> Opts {
        Opts {
            once,
            always,
            verified,
        }
    }

    /// `--always` × source × verification flag: only a global contact without the flag is refused.
    #[test]
    fn always_guard_matrix() {
        for (source, verified, expect) in [
            ("local", false, Some(Mode::Auto)),
            ("local", true, Some(Mode::Auto)),
            ("global", true, Some(Mode::Auto)),
            ("global", false, None),
        ] {
            let got = policy_for(&contact(source), opts(false, true, verified));
            match expect {
                Some(m) => assert_eq!(got.unwrap(), Some(m), "{source}/{verified}"),
                None => {
                    let e = got.unwrap_err();
                    assert_eq!(crate::cli::exit_code(&e), 1);
                    let msg = e.to_string();
                    assert!(msg.contains("--i-verified-the-fingerprint"), "{msg}");
                    assert!(msg.contains("FPFP"), "{msg}");
                }
            }
        }
    }

    /// Default and `--once` never look at the source or the flag.
    #[test]
    fn default_is_manual_and_once_writes_nothing() {
        for source in ["local", "global"] {
            for verified in [false, true] {
                assert_eq!(
                    policy_for(&contact(source), opts(false, false, verified)).unwrap(),
                    Some(Mode::Manual),
                    "{source}/{verified}"
                );
                assert_eq!(
                    policy_for(&contact(source), opts(true, false, verified)).unwrap(),
                    None,
                    "{source}/{verified}"
                );
            }
        }
    }

    #[test]
    fn once_and_always_conflict() {
        for source in ["local", "global"] {
            for verified in [false, true] {
                let e = policy_for(&contact(source), opts(true, true, verified)).unwrap_err();
                assert_eq!(crate::cli::exit_code(&e), 1);
                assert!(e.to_string().contains("mutually exclusive"), "{e}");
            }
        }
    }
}
