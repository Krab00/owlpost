//! `owl show <id|all> [--format plain|claude|codex|kimi]`: the full question or answer, the
//! draft when present; marks seen (§9). `--format claude|codex|kimi` prints the framed
//! Markdown block instead (OWL-032, `owlpost::render`): a question in its orange frame, on
//! a `consent` record with the fingerprint in the header; a drafted one followed by `draft:`,
//! the draft in a plain text block, `harness: <name>` and the language note when the draft's
//! language differs from the question's; an answer framed the same way, project and path
//! taken from the question it replies to. `--json` wins over `--format`. Every shown record
//! is released from the session wake routing (`owlpost::route`, OWL-033).

use std::path::Path;

use anyhow::Context;
use owlpost::envelope::{self, Body};
use owlpost::render;
use owlpost::route;
use owlpost::spool::{Dir, Record, Spool};
use serde_json::{Value, json};

use super::inbox::Format;
use super::{StoredDraft, kind_str, payload_of, peer_name, print_json, summary};

pub fn run(home: &Path, id: &str, json: bool, format: Option<&str>) -> anyhow::Result<()> {
    let format = format.map(Format::parse).transpose()?;
    // `--json` wins: the json branch below is checked first.
    let framed = matches!(format, Some(Format::Claude | Format::Codex | Format::Kimi));
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
        } else if framed {
            if i > 0 {
                println!();
            }
            println!(
                "{}",
                render::record_block(&spool, &book, rec, &payload, draft.as_ref())
            );
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
                    context,
                } => {
                    println!("project:  {project}");
                    println!("path:     {}", path.as_deref().unwrap_or("-"));
                    println!("question:");
                    println!("{question}");
                    // OWL-034: the asker's snippet after the question, when it sent one.
                    if let Some(ctx) = context {
                        println!("context:");
                        println!("{ctx}");
                    }
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
        // Shown: no session needs to wake for it (OWL-033).
        route::release(home, id);
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
