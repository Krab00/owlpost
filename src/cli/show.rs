//! `owl show <id|all>`: the full question or answer, the draft when present; marks seen (§9).

use std::path::Path;

use anyhow::Context;
use owlpost::envelope::{self, Body};
use owlpost::spool::{Dir, Record, Spool};
use serde_json::{Value, json};

use super::{StoredDraft, kind_str, payload_of, peer_name, print_json, summary};

pub fn run(home: &Path, id: &str, json: bool) -> anyhow::Result<()> {
    let spool = Spool::new(home)?;
    let book = super::contact_book(home)?;
    let records: Vec<(String, Record)> = if id == "all" {
        spool.list(Dir::Inbox, |_| true)?
    } else {
        let rec = spool
            .get(Dir::Inbox, id)?
            .with_context(|| format!("no inbox record {id}"))?;
        vec![(id.to_string(), rec)]
    };
    let now = envelope::now_unix();
    let mut out = Vec::new();
    for (i, (id, rec)) in records.iter().enumerate() {
        let payload = payload_of(id, rec)?;
        let draft = StoredDraft::from_record(id, rec)?;
        if json {
            let mut v = summary(id, rec, &payload, &book, now);
            v["payload"] = serde_json::to_value(&payload)?;
            v["draft"] = draft.as_ref().map_or(Value::Null, StoredDraft::to_value);
            out.push(v);
        } else {
            if i > 0 {
                println!();
            }
            println!("id:       {id}");
            println!(
                "from:     {} ({})",
                peer_name(&book, &payload.from),
                payload.from
            );
            println!("type:     {}", kind_str(payload.kind));
            println!("state:    {}", rec.state);
            println!("received: {}", rec.received_at);
            match &payload.body {
                Body::Question {
                    project,
                    path,
                    question,
                } => {
                    println!("project:  {project}");
                    println!("path:     {}", path.as_deref().unwrap_or("-"));
                    println!("question:");
                    println!("{question}");
                }
                Body::Answer {
                    answer,
                    harness,
                    redactions,
                    cached,
                } => {
                    if let Some(q) = &payload.in_reply_to {
                        println!("reply to: {q}");
                    }
                    println!("harness:  {harness}");
                    println!("redactions: {redactions}");
                    println!("cached:   {cached}");
                    println!("answer:");
                    println!("{answer}");
                }
            }
            if let Some(d) = &draft {
                println!(
                    "draft ({} via {}, redactions: {}, {}):",
                    d.status, d.harness, d.redactions, d.drafted_at
                );
                println!("{}", d.text);
            }
        }
        if !rec.seen {
            spool.mark_seen(Dir::Inbox, id)?;
        }
    }
    if json {
        print_json(&if id == "all" {
            Value::Array(out)
        } else {
            out.into_iter().next().unwrap_or(json!(null))
        })?;
    }
    Ok(())
}
