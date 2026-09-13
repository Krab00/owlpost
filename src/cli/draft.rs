//! `owl draft <id> [--harness <name> | --text <text> [--agent] | --prompt]`: run the OWL-009
//! runner through the shared `owlpost::answer::draft`, store the draft on the inbox record
//! (state `drafted`), print it with a redaction summary (§3.4, §10). `--text` stores the
//! human's own answer as the draft instead (harness `human`, no redactions); `--text --agent`
//! stores an in-session agent's answer (harness `agent`, redacted like a harness answer).
//! `--prompt` prints the responder prompt for the record and stops — the plugin hands it to
//! the Agent tool and brings the answer back with `--text --agent`.
//!
//! State gate (§8): `pending` and `drafted` (re-draft) are accepted; `consent` is refused
//! with a pointer to `owl allow`; anything else is refused. A `drafted` record whose signed
//! answer already sits in `outbox/` (a `send` that failed after spooling the envelope) is
//! refused too, pointing at `owl send`: re-drafting would replace the draft while the retry
//! ships the old envelope — the same `existing_answer` guard `owl edit` applies. Runner status `timeout` /
//! `extract_failed`: the draft is still stored and printed, a warning goes to stderr and the
//! command exits 1 so a caller can tell the draft needs a look before `owl send`.

use std::path::Path;

use owlpost::answer;
use owlpost::config::Config;
use owlpost::envelope::Kind;
use owlpost::route;
use owlpost::runner::DraftStatus;
use owlpost::spool::{Dir, Spool};
use serde_json::json;

use super::{
    ExitError, StoredDraft, existing_answer, inbox_record, payload_of, print_json, user_error,
};

pub struct Opts<'a> {
    pub harness: Option<&'a str>,
    pub text: Option<String>,
    pub agent: bool,
    pub prompt: bool,
}

pub fn run(home: &Path, id: &str, opts: Opts<'_>, json: bool) -> anyhow::Result<()> {
    let Opts {
        harness,
        text,
        agent,
        prompt,
    } = opts;
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
    if let Some((aid, _)) = existing_answer(&spool, id)? {
        return Err(user_error(format!(
            "record {id} already has its answer spooled as outbox/{aid}.json — run `owl send {id}` to finish it (drafting again would not change what is sent)"
        )));
    }
    if prompt {
        if json {
            print_json(&json!({"id": id, "prompt": answer::prompt(&config, home, id, &rec)?}))?;
        } else {
            print!("{}", answer::prompt(&config, home, id, &rec)?);
        }
        return Ok(());
    }
    // `--text`: the human (or, with `--agent`, the session's agent) wrote the answer; no
    // harness runs.
    let (rec, status) = match text {
        Some(text) if agent => (answer::draft_agent(&config, rec, &text)?, DraftStatus::Ok),
        Some(text) => (answer::draft_text(rec, &text), DraftStatus::Ok),
        None => {
            let (rec, draft) = answer::draft(&config, home, id, rec, harness)?;
            (rec, draft.status)
        }
    };
    let stored = StoredDraft::from_record(id, &rec)?.expect("a drafted record has a draft");
    spool.put(Dir::Inbox, id, &rec)?;
    // Being handled: no session needs to wake for it (OWL-033).
    route::release(home, id);

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
    match status {
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
