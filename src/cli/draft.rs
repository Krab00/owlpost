//! `owl draft <id> [--harness <name>]`: run the OWL-009 runner through the shared
//! `owlpost::answer::draft`, store the draft on the inbox record (state `drafted`), print it
//! with a redaction summary (§3.4, §10).
//!
//! State gate (§8): `pending` and `drafted` (re-draft) are accepted; `consent` is refused
//! with a pointer to `owl allow`; anything else is refused. Runner status `timeout` /
//! `extract_failed`: the draft is still stored and printed, a warning goes to stderr and the
//! command exits 1 so a caller can tell the draft needs a look before `owl send`.

use std::path::Path;

use owlpost::answer;
use owlpost::config::Config;
use owlpost::envelope::Kind;
use owlpost::runner::DraftStatus;
use owlpost::spool::{Dir, Spool};
use serde_json::json;

use super::{ExitError, StoredDraft, inbox_record, payload_of, print_json, user_error};

pub fn run(home: &Path, id: &str, harness: Option<&str>, json: bool) -> anyhow::Result<()> {
    let config = Config::load(home)?;
    let spool = Spool::new(home)?;
    let rec = inbox_record(&spool, id)?;
    let payload = payload_of(id, &rec)?;
    if payload.kind != Kind::Question {
        return Err(user_error(format!(
            "record {id} is an answer, not a question — nothing to draft"
        )));
    }
    match rec.state.as_str() {
        "pending" | "drafted" => {}
        "consent" => {
            return Err(user_error(format!(
                "record {id} is held for consent — run `owl allow {}` first",
                payload.from
            )));
        }
        other => {
            return Err(user_error(format!(
                "record {id} is in state {other}; only pending or drafted records can be drafted"
            )));
        }
    }
    let (rec, draft) = answer::draft(&config, home, id, rec, harness)?;
    let stored = StoredDraft::from_runner(&draft);
    spool.put(Dir::Inbox, id, &rec)?;

    if json {
        let mut v = rec.draft.clone().unwrap_or_default();
        if let Some(obj) = v.as_object_mut() {
            obj.insert("id".into(), json!(id));
            obj.insert("state".into(), json!("drafted"));
        }
        print_json(&v)?;
    } else {
        println!("{}", stored.text);
        println!("harness: {}", stored.harness);
        println!("redactions: {}", stored.redactions);
        println!("state: drafted ({id})");
    }
    match draft.status {
        DraftStatus::Ok => Ok(()),
        status => Err(ExitError {
            code: 1,
            message: format!(
                "draft for {id} stored with status {}; review it with `owl edit {id}` before `owl send {id}`",
                status.as_str()
            ),
        }
        .into()),
    }
}
