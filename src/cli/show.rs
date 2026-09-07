//! `owl show <id|all> [--format plain|claude|codex|kimi]`: the full question or answer, the
//! draft when present; marks seen (§9). `--format claude|codex|kimi` prints the framed
//! Markdown block instead (OWL-032, `owlpost::render`): a question in its orange frame, on
//! a `consent` record with the fingerprint in the header; a drafted one followed by `draft:`,
//! the draft in a plain text block, `harness: <name>` and the language note when the draft's
//! language differs from the question's; an answer framed the same way, project and path
//! taken from the question it replies to. `--json` wins over `--format`.

use std::path::Path;

use anyhow::Context;
use owlpost::contacts::ContactBook;
use owlpost::envelope::{self, Body, Payload};
use owlpost::render;
use owlpost::spool::{Dir, Record, Spool};
use serde_json::{Value, json};

use super::inbox::Format;
use super::{StoredDraft, kind_str, payload_of, peer_name, print_json, summary};

/// The `--format claude` block for one record.
pub fn render_record(
    spool: &Spool,
    book: &ContactBook,
    rec: &Record,
    payload: &Payload,
    draft: Option<&StoredDraft>,
) -> String {
    let name = peer_name(book, &payload.from);
    let hh_mm = render::local_hh_mm(&rec.received_at);
    let fingerprint = (rec.state == "consent").then_some(payload.from.as_str());
    match &payload.body {
        Body::Question {
            project,
            path,
            question,
        } => {
            let header = render::header(&name, fingerprint, &hh_mm, project, path.as_deref());
            let mut out = render::frame(&header, question);
            if let Some(d) = draft {
                out.push('\n');
                out.push_str(&render::draft_block(&d.text));
                out.push_str(&format!("\nharness: {}", d.harness));
                if let Some(note) = render::language_note(question, &d.text) {
                    out.push('\n');
                    out.push_str(note);
                }
            }
            out
        }
        Body::Answer { answer, .. } => {
            let question = payload
                .in_reply_to
                .as_deref()
                .and_then(|q| render::find_question(spool, q));
            let (project, path) = match question.as_ref().map(|q| &q.body) {
                Some(Body::Question { project, path, .. }) => (project.as_str(), path.as_deref()),
                _ => ("-", None),
            };
            render::frame(
                &render::header(&name, fingerprint, &hh_mm, project, path),
                answer,
            )
        }
    }
}

pub fn run(home: &Path, id: &str, json: bool, format: Option<&str>) -> anyhow::Result<()> {
    let format = format.map(Format::parse).transpose()?;
    let framed = !json && matches!(format, Some(Format::Claude | Format::Codex | Format::Kimi));
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
                render_record(&spool, &book, rec, &payload, draft.as_ref())
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
