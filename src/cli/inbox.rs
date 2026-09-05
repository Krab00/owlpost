//! `owl inbox [--count] [--new] [--all] [--format plain|claude|codex|kimi]` (§8, §9, §3.3).
//!
//! Listing: every inbox record by default (§3.3 "lists everything and marks it seen"), only
//! unseen ones with `--new`; `--all` is accepted for symmetry and lists everything too. Every
//! listed record is marked `seen`. Counting: `--count` prints the unseen count (`--all`: the
//! total) and never marks anything; with `--format` it prints the harness injection line
//! instead, or nothing at all when the count is 0.
//!
//! `--session-start` (OWL-023, set by the plugin's `SessionStart` hook only) adds the
//! `ARM_SENTENCE` to the `claude` injection unless `$OWLPOST_HOME/plugin.json` says
//! `{"watch": false}`; with the count at 0 the sentence stands alone. It is ignored by every
//! other format and without `--count --format`. The same flag adds one preview line per
//! unseen record (`- <name> <kind> [<state>] on <path>: <first line>`) and the
//! `OPEN_SENTENCE` so the first turn opens the inbox instead of reporting a counter; the
//! preview never marks anything seen.
//!
//! Consent records get a prompt line under the table (`<name> wants to ask your agent about
//! <project> — owl allow <fp> [--once|--always] / owl deny <fp>`); records the auto-accept
//! scheduler failed on show their `auto_error` (§3.4) with the command that retries them:
//! `owl send <id>` when the signed answer already sits in `outbox/` (`owl draft` refuses
//! then), `owl draft <id>` otherwise.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::bail;
use owlpost::answer::auto_error;
use owlpost::contacts::ContactBook;
use owlpost::envelope::{self, Body, Kind, Payload};
use owlpost::spool::{Dir, Record, Spool};
use serde::Serialize;
use serde_json::{Value, json};

use super::{existing_answer, payload_of, peer_name, print_json, summary};

pub struct Opts {
    pub count: bool,
    pub new: bool,
    pub all: bool,
    pub format: Option<String>,
    pub json: bool,
    pub session_start: bool,
    pub hook_event: String,
}

/// The second `additionalContext` sentence of the `claude` injection at session start (§9):
/// the skill arms the Monitor-based inbox watch once when it sees it.
pub const ARM_SENTENCE: &str = "owlpost: arm the inbox watch (see /owlpost:watch)";

/// Closes the session-start preview: the skill runs `/owlpost:inbox` on the first turn.
pub const OPEN_SENTENCE: &str = "owlpost: run /owlpost:inbox now.";

/// One session-start preview line for an unseen record.
pub fn preview(rec: &Record, payload: &Payload, book: &ContactBook) -> String {
    let (kind, text) = match &payload.body {
        Body::Question { question, .. } => ("question", question),
        Body::Answer { answer, .. } => ("answer", answer),
    };
    // ponytail: first line, 200 chars; `/owlpost:inbox` shows the rest.
    let first: String = text
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .chars()
        .take(200)
        .collect();
    let at = match super::body_path(&payload.body) {
        "-" => String::new(),
        p => format!(" on {p}"),
    };
    format!(
        "- {} {kind} [{}]{at}: {first}",
        peer_name(book, &payload.from),
        rec.state
    )
}

/// `$OWLPOST_HOME/plugin.json` → `watch`; absent file, unparseable JSON or a missing/non-bool
/// key all mean "on". Written by `/owlpost:watch on|off`, read only here.
pub fn watch_enabled(home: &Path) -> bool {
    std::fs::read(home.join("plugin.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.get("watch")?.as_bool())
        .unwrap_or(true)
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
/// Peers are ordered by count (descending), then name. Singular below two. `answers` is how
/// many of the counted records are answers: all answers reads `1 new answer`, a mix reads
/// `2 new questions, 1 new answer`.
pub fn sentence(per_peer: &[(String, usize)], answers: usize) -> String {
    let total: usize = per_peer.iter().map(|(_, n)| n).sum();
    let questions = total.saturating_sub(answers);
    let plural = |n: usize, one: &str, many: &str| if n == 1 { one } else { many }.to_string();
    let what = match (questions, answers) {
        (_, 0) => format!("{total} new {}", plural(total, "question", "questions")),
        (0, a) => format!("{a} new {}", plural(a, "answer", "answers")),
        (q, a) => format!(
            "{q} new {}, {a} new {}",
            plural(q, "question", "questions"),
            plural(a, "answer", "answers")
        ),
    };
    let peers = per_peer
        .iter()
        .map(|(name, n)| format!("{name} {n}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("owlpost: {what} ({peers}). Say \"show owlpost inbox\" or run `owl inbox`.")
}

/// The exact §9 injection line for `format`; `None` when there is nothing to inject. With
/// `arm`, `Format::Claude` adds the [`ARM_SENTENCE`]: after the counter sentence
/// (space-separated) when there is one, alone when the count is 0. Non-empty `previews`
/// (session start with unseen records) follow on their own lines, closed by
/// [`OPEN_SENTENCE`]. Other formats ignore `arm` and `previews`.
pub fn injection(
    format: Format,
    per_peer: &[(String, usize)],
    answers: usize,
    arm: bool,
    previews: &[String],
    event: &str,
) -> Option<String> {
    let arm = arm && format == Format::Claude;
    let empty = per_peer.iter().all(|(_, n)| *n == 0);
    if empty && !arm {
        return None;
    }
    let mut text = if empty {
        String::new()
    } else {
        sentence(per_peer, answers)
    };
    if arm {
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(ARM_SENTENCE);
    }
    if format == Format::Claude && !empty && !previews.is_empty() {
        for p in previews {
            text.push('\n');
            text.push_str(p);
        }
        text.push('\n');
        text.push_str(OPEN_SENTENCE);
    }
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
                hook_event_name: &'a str,
                additional_context: &'a str,
            }
            serde_json::to_string(&Hook {
                hook_specific_output: Inner {
                    hook_event_name: event,
                    additional_context: &text,
                },
            })
            .expect("hook line is always serialisable")
        }
        Format::Kimi | Format::Plain => text,
    })
}

/// The §3.2 consent prompt for a held question: who, about which project, and the two commands.
pub fn consent_prompt(book: &ContactBook, payload: &Payload) -> String {
    let project = match &payload.body {
        Body::Question { project, .. } => project.as_str(),
        Body::Answer { .. } => "?",
    };
    let fp = &payload.from;
    format!(
        "{} wants to ask your agent about {project} — owl allow {fp} [--once|--always] / owl deny {fp}",
        peer_name(book, fp)
    )
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
        return count(&spool, &book, &opts, format, watch_enabled(home));
    }
    let records = spool.list(Dir::Inbox, |r| !opts.new || !r.seen)?;
    let now = envelope::now_unix();
    let mut rows = Vec::new();
    let mut notes = Vec::new();
    for (id, rec) in &records {
        let payload = payload_of(id, rec)?;
        let mut row = summary(id, rec, &payload, &book, now);
        let error = auto_error(rec);
        if let Some(obj) = row.as_object_mut() {
            obj.insert("auto_error".into(), json!(error));
        }
        rows.push(row);
        if rec.state == "consent" {
            notes.push(consent_prompt(&book, &payload));
        }
        if let Some(e) = error {
            let retry = if existing_answer(&spool, id)?.is_some() {
                "send"
            } else {
                "draft"
            };
            notes.push(format!(
                "{id}: auto-accept failed ({}): {e} — owl {retry} {id} to retry by hand",
                peer_name(&book, &payload.from)
            ));
        }
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
        let styles: Vec<&str> = rows
            .iter()
            .map(|r| {
                super::row_style(
                    r["type"].as_str().unwrap_or_default(),
                    r["seen"].as_bool().unwrap_or(true),
                )
            })
            .collect();
        super::print_table_styled(
            &["ID", "FROM", "TYPE", "STATE", "PATH", "AGE"],
            &cells,
            &styles,
        );
        for n in &notes {
            println!("{n}");
        }
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
    opts: &Opts,
    format: Option<Format>,
    watch: bool,
) -> anyhow::Result<()> {
    let Opts {
        all,
        json,
        session_start,
        ..
    } = *opts;
    let event = &opts.hook_event;
    let records = spool.list(Dir::Inbox, |r| all || !r.seen)?;
    let per_peer = per_peer(&records, book)?;
    let total = records.len();
    let questions = records
        .iter()
        .filter(|(id, r)| payload_of(id, r).is_ok_and(|p| p.kind == Kind::Question))
        .count();
    let previews = if session_start {
        records
            .iter()
            .map(|(id, r)| Ok(preview(r, &payload_of(id, r)?, book)))
            .collect::<anyhow::Result<Vec<_>>>()?
    } else {
        Vec::new()
    };
    match format {
        Some(f) => {
            let arm = session_start && watch;
            if let Some(line) = injection(f, &per_peer, total - questions, arm, &previews, event) {
                println!("{line}");
            }
        }
        None if json => {
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
            sentence(&peers(&[("Maciek", 2)]), 0),
            "owlpost: 2 new questions (Maciek 2). Say \"show owlpost inbox\" or run `owl inbox`."
        );
        assert_eq!(
            sentence(&peers(&[("Maciek", 1)]), 0),
            "owlpost: 1 new question (Maciek 1). Say \"show owlpost inbox\" or run `owl inbox`."
        );
        assert_eq!(
            sentence(&peers(&[("Ana", 2), ("Maciek", 1)]), 0),
            "owlpost: 3 new questions (Ana 2, Maciek 1). Say \"show owlpost inbox\" or run `owl inbox`."
        );
        assert_eq!(
            sentence(&peers(&[("Maciek", 1)]), 1),
            "owlpost: 1 new answer (Maciek 1). Say \"show owlpost inbox\" or run `owl inbox`."
        );
        assert_eq!(
            sentence(&peers(&[("Ana", 2), ("Maciek", 1)]), 1),
            "owlpost: 2 new questions, 1 new answer (Ana 2, Maciek 1). Say \"show owlpost inbox\" or run `owl inbox`."
        );
    }

    #[test]
    fn injection_shapes_per_format() {
        let p = peers(&[("Maciek", 2)]);
        let claude = injection(Format::Claude, &p, 0, false, &[], "UserPromptSubmit").unwrap();
        assert_eq!(
            claude,
            r#"{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"owlpost: 2 new questions (Maciek 2). Say \"show owlpost inbox\" or run `owl inbox`."}}"#
        );
        assert_eq!(
            injection(Format::Codex, &p, 0, false, &[], "UserPromptSubmit").unwrap(),
            claude
        );
        assert_eq!(
            injection(Format::Kimi, &p, 0, false, &[], "UserPromptSubmit").unwrap(),
            sentence(&p, 0)
        );
        assert_eq!(
            injection(Format::Plain, &p, 0, false, &[], "UserPromptSubmit").unwrap(),
            sentence(&p, 0)
        );
        for f in [Format::Plain, Format::Claude, Format::Codex, Format::Kimi] {
            assert_eq!(injection(f, &[], 0, false, &[], "UserPromptSubmit"), None);
            assert_eq!(
                injection(
                    f,
                    &peers(&[("Maciek", 0)]),
                    0,
                    false,
                    &[],
                    "UserPromptSubmit"
                ),
                None
            );
        }
    }

    #[test]
    fn arm_sentence_is_claude_only_and_stands_alone_at_zero() {
        const ARMED_TWO: &str = r#"{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"owlpost: 2 new questions (Maciek 2). Say \"show owlpost inbox\" or run `owl inbox`. owlpost: arm the inbox watch (see /owlpost:watch)"}}"#;
        const ARMED_ZERO: &str = r#"{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"owlpost: arm the inbox watch (see /owlpost:watch)"}}"#;
        assert_eq!(
            ARM_SENTENCE,
            "owlpost: arm the inbox watch (see /owlpost:watch)"
        );
        let p = peers(&[("Maciek", 2)]);
        // Counter + arm, one space between the two sentences, counter text unchanged.
        assert_eq!(
            injection(Format::Claude, &p, 0, true, &[], "SessionStart").unwrap(),
            ARMED_TWO
        );
        // Zero unseen + arm: the arm sentence alone, in the same JSON shape.
        assert_eq!(
            injection(Format::Claude, &[], 0, true, &[], "SessionStart").unwrap(),
            ARMED_ZERO
        );
        assert_eq!(
            injection(
                Format::Claude,
                &peers(&[("Maciek", 0)]),
                0,
                true,
                &[],
                "SessionStart"
            )
            .unwrap(),
            ARMED_ZERO
        );
        // Every other format ignores `arm`: same as without, nothing at zero.
        for f in [Format::Plain, Format::Codex, Format::Kimi] {
            assert_eq!(
                injection(f, &p, 0, true, &[], "SessionStart"),
                injection(f, &p, 0, false, &[], "SessionStart"),
                "{f:?}"
            );
            assert_eq!(
                injection(f, &[], 0, true, &[], "SessionStart"),
                None,
                "{f:?}"
            );
        }
    }

    #[test]
    fn previews_follow_arm_and_close_with_open_sentence_claude_only() {
        let p = peers(&[("Maciek", 1)]);
        let pv = vec!["- Maciek question [consent] on src/a.rs: why?".to_string()];
        let line = injection(Format::Claude, &p, 0, true, &pv, "SessionStart").unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            v["hookSpecificOutput"]["additionalContext"],
            format!(
                "{} {ARM_SENTENCE}\n{}\n{OPEN_SENTENCE}",
                sentence(&p, 0),
                pv[0]
            )
        );
        // Watch off: previews still go out, without the arm sentence.
        let line = injection(Format::Claude, &p, 0, false, &pv, "SessionStart").unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            v["hookSpecificOutput"]["additionalContext"],
            format!("{}\n{}\n{OPEN_SENTENCE}", sentence(&p, 0), pv[0])
        );
        // Zero unseen: no previews, no open sentence.
        assert_eq!(
            injection(Format::Claude, &[], 0, true, &pv, "SessionStart").unwrap(),
            injection(Format::Claude, &[], 0, true, &[], "SessionStart").unwrap()
        );
        for f in [Format::Plain, Format::Codex, Format::Kimi] {
            assert_eq!(
                injection(f, &p, 0, false, &pv, "SessionStart"),
                injection(f, &p, 0, false, &[], "SessionStart"),
                "{f:?}"
            );
        }
    }

    #[test]
    fn watch_enabled_defaults_on_and_reads_plugin_json() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let file = home.join("plugin.json");
        assert!(watch_enabled(home), "absent file");
        std::fs::write(&file, r#"{"watch": false}"#).unwrap();
        assert!(!watch_enabled(home), "watch false");
        std::fs::write(&file, r#"{"watch": true}"#).unwrap();
        assert!(watch_enabled(home), "watch true");
        std::fs::write(&file, r#"{"other": false}"#).unwrap();
        assert!(watch_enabled(home), "missing key");
        std::fs::write(&file, r#"{"watch": "false"}"#).unwrap();
        assert!(watch_enabled(home), "non-bool value");
        std::fs::write(&file, "{not json").unwrap();
        assert!(watch_enabled(home), "unparseable");
        std::fs::write(&file, "").unwrap();
        assert!(watch_enabled(home), "empty file");
    }

    #[test]
    fn consent_prompt_names_peer_project_and_both_commands() {
        use owlpost::contacts::Contact;
        let q = Payload::question("FPA", "FPB", "github.com/x/y", Some("src/a.rs"), "why?");
        let book = ContactBook {
            contacts: vec![Contact {
                name: "Ana".into(),
                emails: vec![],
                pubkey: "ed25519:x".into(),
                endpoints: vec![],
                source: "local".into(),
                policy: None,
                added_at: None,
                fingerprint: "FPA".into(),
            }],
        };
        assert_eq!(
            consent_prompt(&book, &q),
            "Ana wants to ask your agent about github.com/x/y — owl allow FPA [--once|--always] / owl deny FPA"
        );
        // Unknown peer: the fingerprint stands in for the name.
        assert_eq!(
            consent_prompt(&ContactBook::default(), &q),
            "FPA wants to ask your agent about github.com/x/y — owl allow FPA [--once|--always] / owl deny FPA"
        );
        let a = Payload::answer(&q, "because", "fake", 0, false);
        assert!(consent_prompt(&book, &a).contains("about ? —"));
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
