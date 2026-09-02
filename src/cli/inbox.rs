//! `owl inbox [--count] [--new] [--all] [--format plain|claude|codex|kimi]` (§8, §9, §3.3).
//!
//! Listing: every inbox record by default (§3.3 "lists everything and marks it seen"), only
//! unseen ones with `--new`; `--all` is accepted for symmetry and lists everything too. Every
//! listed record is marked `seen`. Counting: `--count` prints the unseen count (`--all`: the
//! total) and never marks anything; with `--format` it prints the harness injection line
//! instead, or nothing at all when the count is 0.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::bail;
use owlpost::contacts::ContactBook;
use owlpost::envelope::{self, Kind};
use owlpost::spool::{Dir, Record, Spool};
use serde::Serialize;
use serde_json::{Value, json};

use super::{payload_of, peer_name, print_json, print_table, summary};

pub struct Opts {
    pub count: bool,
    pub new: bool,
    pub all: bool,
    pub format: Option<String>,
    pub json: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Plain,
    Claude,
    Codex,
    Kimi,
}

impl Format {
    pub fn parse(s: &str) -> anyhow::Result<Format> {
        Ok(match s {
            "plain" => Format::Plain,
            "claude" => Format::Claude,
            "codex" => Format::Codex,
            "kimi" => Format::Kimi,
            other => bail!("unknown --format {other}: expected plain|claude|codex|kimi"),
        })
    }
}

/// `owlpost: 2 new questions (Maciek 2). Say "show owlpost inbox" or run `owl inbox`.`
/// Peers are ordered by count (descending), then name. Singular below two.
pub fn sentence(per_peer: &[(String, usize)]) -> String {
    let total: usize = per_peer.iter().map(|(_, n)| n).sum();
    let noun = if total == 1 { "question" } else { "questions" };
    let peers = per_peer
        .iter()
        .map(|(name, n)| format!("{name} {n}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("owlpost: {total} new {noun} ({peers}). Say \"show owlpost inbox\" or run `owl inbox`.")
}

/// The exact §9 injection line for `format`; `None` when there is nothing to inject.
pub fn injection(format: Format, per_peer: &[(String, usize)]) -> Option<String> {
    if per_peer.iter().all(|(_, n)| *n == 0) {
        return None;
    }
    let text = sentence(per_peer);
    Some(match format {
        Format::Claude | Format::Codex => {
            // Structs keep the §9 key order; `json!` maps would sort keys alphabetically.
            #[derive(Serialize)]
            #[serde(rename_all = "camelCase")]
            struct Hook<'a> {
                hook_specific_output: Inner<'a>,
            }
            #[derive(Serialize)]
            #[serde(rename_all = "camelCase")]
            struct Inner<'a> {
                hook_event_name: &'static str,
                additional_context: &'a str,
            }
            serde_json::to_string(&Hook {
                hook_specific_output: Inner {
                    hook_event_name: "UserPromptSubmit",
                    additional_context: &text,
                },
            })
            .expect("hook line is always serialisable")
        }
        Format::Kimi | Format::Plain => text,
    })
}

/// Per-peer counts over `records`, named through the contact book, ordered by count then name.
pub fn per_peer(
    records: &[(String, Record)],
    book: &ContactBook,
) -> anyhow::Result<Vec<(String, usize)>> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (id, rec) in records {
        let p = payload_of(id, rec)?;
        *counts.entry(peer_name(book, &p.from)).or_default() += 1;
    }
    let mut v: Vec<(String, usize)> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Ok(v)
}

pub fn run(home: &Path, opts: Opts) -> anyhow::Result<()> {
    let format = opts.format.as_deref().map(Format::parse).transpose()?;
    let spool = Spool::new(home)?;
    let book = super::contact_book(home)?;
    if opts.count {
        return count(&spool, &book, opts.all, format, opts.json);
    }
    let records = spool.list(Dir::Inbox, |r| !opts.new || !r.seen)?;
    let now = envelope::now_unix();
    let mut rows = Vec::new();
    for (id, rec) in &records {
        let payload = payload_of(id, rec)?;
        rows.push(summary(id, rec, &payload, &book, now));
    }
    if opts.json {
        print_json(&Value::Array(rows.clone()))?;
    } else {
        let cells: Vec<Vec<String>> = rows
            .iter()
            .map(|r| {
                ["id", "from_name", "type", "state", "path", "age"]
                    .iter()
                    .map(|k| r[k].as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .collect();
        print_table(&["ID", "FROM", "TYPE", "STATE", "PATH", "AGE"], &cells);
    }
    for (id, rec) in &records {
        if !rec.seen {
            spool.mark_seen(Dir::Inbox, id)?;
        }
    }
    Ok(())
}

fn count(
    spool: &Spool,
    book: &ContactBook,
    all: bool,
    format: Option<Format>,
    json: bool,
) -> anyhow::Result<()> {
    let records = spool.list(Dir::Inbox, |r| all || !r.seen)?;
    let per_peer = per_peer(&records, book)?;
    let total = records.len();
    match format {
        Some(f) => {
            if let Some(line) = injection(f, &per_peer) {
                println!("{line}");
            }
        }
        None if json => {
            let questions = records
                .iter()
                .filter(|(id, r)| payload_of(id, r).is_ok_and(|p| p.kind == Kind::Question))
                .count();
            print_json(&json!({
                "count": total,
                "questions": questions,
                "peers": per_peer.iter().map(|(n, c)| json!({"name": n, "count": c})).collect::<Vec<_>>(),
            }))?;
        }
        None => println!("{total}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peers(v: &[(&str, usize)]) -> Vec<(String, usize)> {
        v.iter().map(|(n, c)| (n.to_string(), *c)).collect()
    }

    #[test]
    fn sentence_matches_design_literal() {
        assert_eq!(
            sentence(&peers(&[("Maciek", 2)])),
            "owlpost: 2 new questions (Maciek 2). Say \"show owlpost inbox\" or run `owl inbox`."
        );
        assert_eq!(
            sentence(&peers(&[("Maciek", 1)])),
            "owlpost: 1 new question (Maciek 1). Say \"show owlpost inbox\" or run `owl inbox`."
        );
        assert_eq!(
            sentence(&peers(&[("Ana", 2), ("Maciek", 1)])),
            "owlpost: 3 new questions (Ana 2, Maciek 1). Say \"show owlpost inbox\" or run `owl inbox`."
        );
    }

    #[test]
    fn injection_shapes_per_format() {
        let p = peers(&[("Maciek", 2)]);
        let claude = injection(Format::Claude, &p).unwrap();
        assert_eq!(
            claude,
            r#"{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"owlpost: 2 new questions (Maciek 2). Say \"show owlpost inbox\" or run `owl inbox`."}}"#
        );
        assert_eq!(injection(Format::Codex, &p).unwrap(), claude);
        assert_eq!(injection(Format::Kimi, &p).unwrap(), sentence(&p));
        assert_eq!(injection(Format::Plain, &p).unwrap(), sentence(&p));
        for f in [Format::Plain, Format::Claude, Format::Codex, Format::Kimi] {
            assert_eq!(injection(f, &[]), None);
            assert_eq!(injection(f, &peers(&[("Maciek", 0)])), None);
        }
    }

    #[test]
    fn format_parse_rejects_unknown() {
        assert_eq!(Format::parse("claude").unwrap(), Format::Claude);
        assert_eq!(Format::parse("codex").unwrap(), Format::Codex);
        assert_eq!(Format::parse("kimi").unwrap(), Format::Kimi);
        assert_eq!(Format::parse("plain").unwrap(), Format::Plain);
        let err = Format::parse("Claude").unwrap_err().to_string();
        assert!(err.contains("unknown --format Claude"), "{err}");
    }
}
