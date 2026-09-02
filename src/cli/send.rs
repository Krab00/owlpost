//! `owl send <id>`: sign the answer, write it to `outbox/` (state `unacked`), move the question
//! to `done/` (state `answered`), update the responder cache (§3.4 step 5, §8).
//!
//! Outbox and cache records share one shape — `{raw, sig, state, seen, received_at, draft,
//! meta}` with `meta = {peer, question_id, hash}` — so the daemon's `GET /v1/outbox` listing
//! (`raw`/`sig`, `to` == caller) and its cache-hit path (`cache_get(hash).raw/.sig`) read it
//! unchanged.

use std::path::Path;

use anyhow::Context;
use owlpost::envelope::{self, Body, Envelope, Kind, Payload};
use owlpost::identity::{self, Identity};
use owlpost::spool::{Dir, Record, Spool};
use serde_json::json;

use super::{StoredDraft, finish, inbox_record, payload_of, print_json, user_error};

pub fn run(home: &Path, id: &str, json: bool) -> anyhow::Result<()> {
    let spool = Spool::new(home)?;
    let rec = inbox_record(&spool, id)?;
    let question = payload_of(id, &rec)?;
    if question.kind != Kind::Question {
        return Err(user_error(format!(
            "record {id} is an answer, not a question — nothing to send"
        )));
    }
    let draft = StoredDraft::from_record(id, &rec)?;
    let draft = match (rec.state.as_str(), draft) {
        ("drafted", Some(d)) => d,
        (_, None) => {
            return Err(user_error(format!(
                "record {id} has no draft — run `owl draft {id}` first"
            )));
        }
        (state, Some(_)) => {
            return Err(user_error(format!(
                "record {id} is in state {state}, not drafted — run `owl draft {id}` first"
            )));
        }
    };
    let identity = Identity::load(home).context("loading own key (run `owl init`)")?;
    let own = identity::fingerprint(&identity.verifying_key());
    if question.to != own {
        return Err(user_error(format!(
            "record {id} is addressed to {}, not to this identity ({own})",
            question.to
        )));
    }
    let Body::Question {
        project,
        path,
        question: text,
    } = &question.body
    else {
        return Err(user_error(format!("record {id} has no question body")));
    };
    let hash = envelope::question_hash(project, path, text);

    let answer = Payload::answer(
        &question,
        &draft.text,
        &draft.harness,
        draft.redactions,
        false,
    );
    let env = Envelope::sign(&answer, &identity);
    let now = envelope::rfc3339_now();
    let out = Record {
        raw: env.raw,
        sig: env.sig,
        state: "unacked".into(),
        seen: false,
        received_at: now,
        draft: None,
        meta: json!({ "peer": question.from, "question_id": id, "hash": hash }),
    };
    spool.put(Dir::Outbox, &answer.id, &out)?;
    spool.cache_put(&hash, &out)?;
    finish(
        &spool,
        id,
        rec,
        "answered",
        &[("answer_id", json!(answer.id))],
    )?;

    if json {
        print_json(&json!({
            "id": answer.id,
            "in_reply_to": id,
            "to": answer.to,
            "outbox": spool.path(Dir::Outbox, &answer.id),
            "redactions": draft.redactions,
            "harness": draft.harness,
        }))?;
    } else {
        println!("sent {} (reply to {id}, to {})", answer.id, answer.to);
    }
    Ok(())
}
