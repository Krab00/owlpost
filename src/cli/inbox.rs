//! `owl inbox [--count] [--new] [--all] [--format plain|claude|codex|kimi]` (§8, §9, §3.3).
//!
//! Listing: every inbox record by default (§3.3 "lists everything and marks it seen"), only
//! unseen ones with `--new`; `--all` is accepted for symmetry and lists everything too. Every
//! listed record is marked `seen`. Counting: `--count` prints the unseen count (`--all`: the
//! total) and never marks anything; with `--format` it prints the harness injection line
//! instead, or nothing at all when the count is 0.
//!
//! `--count --format claude` (the plugin hook) reads the hook's stdin JSON when stdin is not
//! a terminal and takes `session_id` from it (OWL-029). The [`arm_sentence`] is added on
//! `--hook-event SessionStart` and `UserPromptSubmit` when `$OWLPOST_HOME/plugin.json` does
//! not say `{"watch": false}` and no live marker `$OWLPOST_HOME/watch/<session id>` exists
//! (a marker whose pid is dead is removed); without a session id the sentence goes out on
//! `SessionStart` only. `--hook-event SessionStart` also adds one preview line per unseen
//! record (`- <name> <kind> [<state>] on <path>: <first line>`) and the `OPEN_SENTENCE` so
//! the first turn opens the inbox instead of reporting a counter; the preview never marks
//! anything seen.
//!
//! `--count --follow [--session <id>]` is the live watch itself (plain format only): with
//! `--session` it writes its pid to the marker, then polls the count every 5 s
//! (`OWLPOST_FOLLOW_SECS`), prints the counter sentence only when it changed, nothing at zero,
//! never marks anything seen, and ends (removing the marker) when the marker is removed from
//! outside or its stdout is closed.
//!
//! Consent records get a prompt line under the table (`<name> wants to ask your agent about
//! <project> — owl allow <fp> [--once|--always] / owl deny <fp>`); records the auto-accept
//! scheduler failed on show their `auto_error` (§3.4) with the command that retries them:
//! `owl send <id>` when the signed answer already sits in `outbox/` (`owl draft` refuses
//! then), `owl draft <id>` otherwise.

use std::collections::BTreeMap;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
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
    pub follow: bool,
    pub session: Option<String>,
    pub hook_event: String,
}

/// The live-watch command template, byte-identical to the fenced line in
/// `plugins/claude-code/commands/watch.md` (OWL-029 AC5; a unit test compares the two). It is
/// stated here once and nowhere else in Rust; [`arm_sentence`] fills in the session id.
pub const WATCH_COMMAND: &str = "owl inbox --count --follow --session <session id>";
/// The placeholder [`WATCH_COMMAND`] carries.
const SESSION_PLACEHOLDER: &str = "<session id>";
/// Every arm sentence starts with this (pinned by the plugin docs and tests).
pub const ARM_PREFIX: &str = "owlpost: before handling this prompt";
/// Session ids: `[A-Za-z0-9._-]{1,128}`, so `watch/<id>` can never leave `watch/`.
pub const SESSION_ID_RULE: &str = "[A-Za-z0-9._-]{1,128}";
/// Poll interval of `--follow` in seconds (fractions allowed); default 5.
pub const FOLLOW_SECS_ENV: &str = "OWLPOST_FOLLOW_SECS";
/// Where a running follow leaves its pid: `$OWLPOST_HOME/watch/<session id>`.
pub const MARKER_DIR: &str = "watch";

/// Prefix of every counter sentence: incoming messages wear the owl (OWL-027).
pub const ICON: &str = "🦉 ";

/// The second `additionalContext` sentence of the `claude` injection (§9): a self-contained
/// imperative (OWL-026) that carries the whole Monitor call, so the turn arms the watch
/// without looking anything up — even when that turn is a slash command. With a session id
/// the command is [`WATCH_COMMAND`] with the id filled in; without one (a harness that sends
/// no stdin) it is the command without `--session`, which runs the same loop without a marker.
pub fn arm_sentence(session: Option<&str>) -> String {
    let command = match session {
        Some(id) => WATCH_COMMAND.replace(SESSION_PLACEHOLDER, id),
        None => WATCH_COMMAND
            .strip_suffix(&format!(" --session {SESSION_PLACEHOLDER}"))
            .expect("WATCH_COMMAND ends with the session flag")
            .to_string(),
    };
    format!(
        "{ARM_PREFIX}, arm the inbox watch for this session: call Monitor with persistent: true, description \"owlpost inbox\" and this command (verbatim): `{command}`. Then handle the prompt."
    )
}

/// `[A-Za-z0-9._-]{1,128}`.
pub fn valid_session_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// clap value parser for `--session`: a usage error (exit 2) for anything outside the rule.
pub fn parse_session_id(s: &str) -> Result<String, String> {
    if valid_session_id(s) {
        Ok(s.to_string())
    } else {
        Err(format!("session id must match {SESSION_ID_RULE}"))
    }
}

/// `$OWLPOST_HOME/watch/<id>`.
pub fn marker_path(home: &Path, id: &str) -> PathBuf {
    home.join(MARKER_DIR).join(id)
}

#[cfg(target_os = "linux")]
fn pid_alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

#[cfg(not(target_os = "linux"))]
fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A marker is live when it holds the pid of a running process.
// ponytail: a recycled pid keeps a stale marker "live" until that process exits — store the
// process start time next to the pid when that shows up.
pub fn marker_is_live(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .is_some_and(|pid| pid > 0 && pid_alive(pid))
}

/// Whether a live follow runs for `id`; a stale marker (dead pid) is removed on the way.
fn live_watch(home: &Path, id: &str) -> bool {
    let marker = marker_path(home, id);
    if !marker.exists() {
        return false;
    }
    if marker_is_live(&marker) {
        return true;
    }
    let _ = std::fs::remove_file(&marker);
    false
}

/// `session_id` from the hook's stdin JSON (`{"session_id": "...", ...}`); `None` on a
/// terminal, on empty or unparseable input, and for an id outside the rule.
pub fn session_id_from_stdin() -> Option<String> {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return None;
    }
    let mut raw = String::new();
    stdin.read_to_string(&mut raw).ok()?;
    session_id_from_json(&raw)
}

/// The validated `session_id` of a hook input JSON text.
pub fn session_id_from_json(raw: &str) -> Option<String> {
    let v: Value = serde_json::from_str(raw).ok()?;
    let id = v.get("session_id")?.as_str()?;
    valid_session_id(id).then(|| id.to_string())
}

/// `OWLPOST_FOLLOW_SECS` as a duration; unset, empty, unparseable or negative → 5 s.
pub fn follow_interval(raw: Option<&std::ffi::OsStr>) -> Duration {
    raw.and_then(|v| v.to_str()?.trim().parse::<f64>().ok())
        .filter(|s| s.is_finite() && *s >= 0.0)
        .map_or(Duration::from_secs(5), Duration::from_secs_f64)
}

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

/// `🦉 owlpost: 2 new questions (Maciek 2). Say "show owlpost inbox" or run `owl inbox`.`
/// The [`ICON`] marks a message from a peer (OWL-027); the arm sentence and the preview
/// lines carry none. Peers are ordered by count (descending), then name. Singular below two. `answers` is how
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
    format!("{ICON}owlpost: {what} ({peers}). Say \"show owlpost inbox\" or run `owl inbox`.")
}

/// The exact §9 injection line for `format`; `None` when there is nothing to inject. With
/// `arm` (the [`arm_sentence`]), `Format::Claude` adds it: after the counter sentence
/// (space-separated) when there is one, alone when the count is 0. Non-empty `previews`
/// (session start with unseen records) follow on their own lines, closed by
/// [`OPEN_SENTENCE`]. Other formats ignore `arm` and `previews`.
pub fn injection(
    format: Format,
    per_peer: &[(String, usize)],
    answers: usize,
    arm: Option<&str>,
    previews: &[String],
    event: &str,
) -> Option<String> {
    let arm = arm.filter(|_| format == Format::Claude);
    let empty = per_peer.iter().all(|(_, n)| *n == 0);
    if empty && arm.is_none() {
        return None;
    }
    let mut text = if empty {
        String::new()
    } else {
        sentence(per_peer, answers)
    };
    if let Some(arm) = arm {
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(arm);
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
    if opts.follow {
        return follow(home, opts.session.as_deref(), opts.all);
    }
    if opts.count {
        return count(home, &spool, &book, &opts, format, watch_enabled(home));
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

/// Unseen (or, with `all`, every) inbox record with the per-peer and question counts.
struct Counts {
    records: Vec<(String, Record)>,
    per_peer: Vec<(String, usize)>,
    questions: usize,
}

fn counts(spool: &Spool, book: &ContactBook, all: bool) -> anyhow::Result<Counts> {
    let records = spool.list(Dir::Inbox, |r| all || !r.seen)?;
    let per_peer = per_peer(&records, book)?;
    let questions = records
        .iter()
        .filter(|(id, r)| payload_of(id, r).is_ok_and(|p| p.kind == Kind::Question))
        .count();
    Ok(Counts {
        records,
        per_peer,
        questions,
    })
}

fn count(
    home: &Path,
    spool: &Spool,
    book: &ContactBook,
    opts: &Opts,
    format: Option<Format>,
    watch: bool,
) -> anyhow::Result<()> {
    let Opts { all, json, .. } = *opts;
    let event = opts.hook_event.as_str();
    let session_start = event == "SessionStart";
    let Counts {
        records,
        per_peer,
        questions,
    } = counts(spool, book, all)?;
    let total = records.len();
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
            let session = if f == Format::Claude {
                session_id_from_stdin()
            } else {
                None
            };
            let arm = watch
                && match session.as_deref() {
                    Some(id) => {
                        matches!(event, "SessionStart" | "UserPromptSubmit")
                            && !live_watch(home, id)
                    }
                    None => session_start,
                };
            let sentence = arm.then(|| arm_sentence(session.as_deref()));
            if let Some(line) = injection(
                f,
                &per_peer,
                total - questions,
                sentence.as_deref(),
                &previews,
                event,
            ) {
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

/// One poll of the follow loop: the plain counter sentence, `None` at zero or on any error
/// (a failed poll prints nothing, like the shell loop it replaced).
fn poll(home: &Path, all: bool) -> Option<String> {
    let spool = Spool::new(home).ok()?;
    let book = super::contact_book(home).ok()?;
    let c = counts(&spool, &book, all).ok()?;
    injection(
        Format::Plain,
        &c.per_peer,
        c.records.len() - c.questions,
        None,
        &[],
        "",
    )
}

/// `owl inbox --count --follow [--session <id>]`: see the module doc.
pub fn follow(home: &Path, session: Option<&str>, all: bool) -> anyhow::Result<()> {
    let marker = session.map(|id| marker_path(home, id));
    if let Some(m) = &marker {
        let dir = m.parent().expect("marker has a parent");
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        std::fs::write(m, format!("{}\n", std::process::id()))
            .with_context(|| format!("writing {}", m.display()))?;
    }
    let interval = follow_interval(std::env::var_os(FOLLOW_SECS_ENV).as_deref());
    let mut prev: Option<String> = None;
    let mut out = std::io::stdout();
    loop {
        let cur = poll(home, all);
        if cur != prev
            && let Some(line) = &cur
            && (writeln!(out, "{line}").is_err() || out.flush().is_err())
        {
            // The reader is gone (the Monitor was stopped): a clean end.
            break;
        }
        prev = cur;
        std::thread::sleep(interval);
        if marker.as_ref().is_some_and(|m| !m.exists()) {
            // Removed from outside: the way to stop a watch without knowing its pid.
            break;
        }
    }
    if let Some(m) = &marker {
        let _ = std::fs::remove_file(m);
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
            "🦉 owlpost: 2 new questions (Maciek 2). Say \"show owlpost inbox\" or run `owl inbox`."
        );
        assert_eq!(
            sentence(&peers(&[("Maciek", 1)]), 0),
            "🦉 owlpost: 1 new question (Maciek 1). Say \"show owlpost inbox\" or run `owl inbox`."
        );
        assert_eq!(
            sentence(&peers(&[("Ana", 2), ("Maciek", 1)]), 0),
            "🦉 owlpost: 3 new questions (Ana 2, Maciek 1). Say \"show owlpost inbox\" or run `owl inbox`."
        );
        assert_eq!(
            sentence(&peers(&[("Maciek", 1)]), 1),
            "🦉 owlpost: 1 new answer (Maciek 1). Say \"show owlpost inbox\" or run `owl inbox`."
        );
        assert_eq!(
            sentence(&peers(&[("Ana", 2), ("Maciek", 1)]), 1),
            "🦉 owlpost: 2 new questions, 1 new answer (Ana 2, Maciek 1). Say \"show owlpost inbox\" or run `owl inbox`."
        );
    }

    #[test]
    fn injection_shapes_per_format() {
        let p = peers(&[("Maciek", 2)]);
        let claude = injection(Format::Claude, &p, 0, None, &[], "UserPromptSubmit").unwrap();
        assert_eq!(
            claude,
            r#"{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"🦉 owlpost: 2 new questions (Maciek 2). Say \"show owlpost inbox\" or run `owl inbox`."}}"#
        );
        assert_eq!(
            injection(Format::Codex, &p, 0, None, &[], "UserPromptSubmit").unwrap(),
            claude
        );
        assert_eq!(
            injection(Format::Kimi, &p, 0, None, &[], "UserPromptSubmit").unwrap(),
            sentence(&p, 0)
        );
        assert_eq!(
            injection(Format::Plain, &p, 0, None, &[], "UserPromptSubmit").unwrap(),
            sentence(&p, 0)
        );
        for f in [Format::Plain, Format::Claude, Format::Codex, Format::Kimi] {
            assert_eq!(injection(f, &[], 0, None, &[], "UserPromptSubmit"), None);
            assert_eq!(
                injection(
                    f,
                    &peers(&[("Maciek", 0)]),
                    0,
                    None,
                    &[],
                    "UserPromptSubmit"
                ),
                None
            );
        }
    }

    /// The exact SessionStart line for `context`: serde's own string escaping, §9 key order.
    fn session_start_line(context: &str) -> String {
        format!(
            r#"{{"hookSpecificOutput":{{"hookEventName":"SessionStart","additionalContext":{}}}}}"#,
            serde_json::to_string(context).unwrap()
        )
    }

    #[test]
    fn arm_sentence_is_self_contained_and_embeds_the_watch_command() {
        // OWL-026 AC1 / OWL-029: the required fragments, in one imperative sentence.
        let armed = arm_sentence(Some("abc-123"));
        assert_eq!(
            armed,
            "owlpost: before handling this prompt, arm the inbox watch for this session: call Monitor with persistent: true, description \"owlpost inbox\" and this command (verbatim): `owl inbox --count --follow --session abc-123`. Then handle the prompt."
        );
        assert!(armed.starts_with(ARM_PREFIX));
        for needle in [
            "persistent: true",
            "description \"owlpost inbox\"",
            "`owl inbox --count --follow --session abc-123`",
        ] {
            assert!(armed.contains(needle), "arm sentence lacks {needle:?}");
        }
        assert!(!armed.contains('\n'), "one line");
        assert!(!armed.contains(SESSION_PLACEHOLDER), "{armed}");
        // Without a session id the same loop runs without a marker.
        let bare = arm_sentence(None);
        assert!(
            bare.contains("`owl inbox --count --follow`. Then handle the prompt."),
            "{bare}"
        );
        assert!(!bare.contains("--session"), "{bare}");
        assert!(bare.starts_with(ARM_PREFIX));
        // OWL-029 AC5: the template is stated once here and once in commands/watch.md; the
        // two are byte-identical (the fenced block holds exactly one line starting
        // `owl inbox --count --follow`).
        let md = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/plugins/claude-code/commands/watch.md"
        ))
        .unwrap();
        let in_md: Vec<&str> = md
            .lines()
            .filter(|l| l.starts_with("owl inbox --count --follow"))
            .collect();
        assert_eq!(in_md, vec![WATCH_COMMAND]);
    }

    #[test]
    fn arm_sentence_is_claude_only_and_stands_alone_at_zero() {
        let p = peers(&[("Maciek", 2)]);
        let arm = arm_sentence(Some("S"));
        // Counter + arm, one space between the two sentences, counter text unchanged.
        let two = injection(Format::Claude, &p, 0, Some(&arm), &[], "SessionStart").unwrap();
        assert_eq!(
            two,
            session_start_line(&format!("{} {arm}", sentence(&p, 0)))
        );
        // Valid JSON despite the `"` in the sentence, with the sentence intact.
        let v: serde_json::Value = serde_json::from_str(&two).unwrap();
        assert_eq!(
            v,
            json!({"hookSpecificOutput": {"hookEventName": "SessionStart",
                "additionalContext": format!("{} {arm}", sentence(&p, 0))}})
        );
        // Zero unseen + arm: the arm sentence alone, in the same JSON shape.
        let zero = session_start_line(&arm);
        assert_eq!(
            injection(Format::Claude, &[], 0, Some(&arm), &[], "SessionStart").unwrap(),
            zero
        );
        assert_eq!(
            injection(
                Format::Claude,
                &peers(&[("Maciek", 0)]),
                0,
                Some(&arm),
                &[],
                "SessionStart"
            )
            .unwrap(),
            zero
        );
        // Every other format ignores `arm`: same as without, nothing at zero.
        for f in [Format::Plain, Format::Codex, Format::Kimi] {
            assert_eq!(
                injection(f, &p, 0, Some(&arm), &[], "SessionStart"),
                injection(f, &p, 0, None, &[], "SessionStart"),
                "{f:?}"
            );
            assert_eq!(
                injection(f, &[], 0, Some(&arm), &[], "SessionStart"),
                None,
                "{f:?}"
            );
        }
    }

    #[test]
    fn previews_follow_arm_and_close_with_open_sentence_claude_only() {
        let p = peers(&[("Maciek", 1)]);
        let arm = arm_sentence(Some("S"));
        let pv = vec!["- Maciek question [consent] on src/a.rs: why?".to_string()];
        let line = injection(Format::Claude, &p, 0, Some(&arm), &pv, "SessionStart").unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            v["hookSpecificOutput"]["additionalContext"],
            format!("{} {arm}\n{}\n{OPEN_SENTENCE}", sentence(&p, 0), pv[0])
        );
        // Watch off: previews still go out, without the arm sentence.
        let line = injection(Format::Claude, &p, 0, None, &pv, "SessionStart").unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            v["hookSpecificOutput"]["additionalContext"],
            format!("{}\n{}\n{OPEN_SENTENCE}", sentence(&p, 0), pv[0])
        );
        // Zero unseen: no previews, no open sentence.
        assert_eq!(
            injection(Format::Claude, &[], 0, Some(&arm), &pv, "SessionStart").unwrap(),
            injection(Format::Claude, &[], 0, Some(&arm), &[], "SessionStart").unwrap()
        );
        for f in [Format::Plain, Format::Codex, Format::Kimi] {
            assert_eq!(
                injection(f, &p, 0, None, &pv, "SessionStart"),
                injection(f, &p, 0, None, &[], "SessionStart"),
                "{f:?}"
            );
        }
    }

    #[test]
    fn session_ids_follow_the_rule() {
        for ok in ["a", "abc-123_x.y", &"z".repeat(128), "0", "A-B"] {
            assert!(valid_session_id(ok), "{ok:?}");
            assert_eq!(parse_session_id(ok).unwrap(), ok);
        }
        for bad in [
            "",
            &"z".repeat(129),
            "a b",
            "../x",
            "a/b",
            "ü",
            "a\n",
            "$(x)",
            "<session id>",
        ] {
            assert!(!valid_session_id(bad), "{bad:?}");
            let err = parse_session_id(bad).unwrap_err();
            assert!(err.contains(SESSION_ID_RULE), "{err}");
        }
        // The marker never leaves `watch/`: the path is home/watch/<id> for a valid id.
        assert_eq!(
            marker_path(Path::new("/h"), "abc"),
            PathBuf::from("/h/watch/abc")
        );
    }

    #[test]
    fn session_id_from_json_takes_only_a_valid_session_id() {
        assert_eq!(
            session_id_from_json(r#"{"session_id":"S1","hook_event_name":"SessionStart"}"#),
            Some("S1".to_string())
        );
        for raw in [
            "",
            "not json",
            "{}",
            r#"{"session_id": 5}"#,
            r#"{"session_id": ""}"#,
            r#"{"session_id": "a/b"}"#,
            r#"{"sessionId": "S1"}"#,
            "[]",
        ] {
            assert_eq!(session_id_from_json(raw), None, "{raw:?}");
        }
    }

    #[test]
    fn marker_is_live_only_for_a_running_pid() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("m");
        // A spawned child that is still running: live.
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        std::fs::write(&marker, format!("{}\n", child.id())).unwrap();
        assert!(marker_is_live(&marker));
        // The same pid once the child has exited and been reaped: dead.
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(!marker_is_live(&marker));
        // A pid that cannot exist, a non-number, zero, an empty or missing file: dead.
        for body in ["4194303", "999999999", "abc", "0", "", "-1", "1.5"] {
            std::fs::write(&marker, body).unwrap();
            assert!(!marker_is_live(&marker), "{body:?}");
        }
        std::fs::remove_file(&marker).unwrap();
        assert!(!marker_is_live(&marker));
        // Our own pid: live.
        std::fs::write(&marker, std::process::id().to_string()).unwrap();
        assert!(marker_is_live(&marker));
    }

    #[test]
    fn live_watch_ignores_and_removes_a_stale_marker() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        assert!(!live_watch(home, "S"), "no marker");
        let marker = marker_path(home, "S");
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, std::process::id().to_string()).unwrap();
        assert!(live_watch(home, "S"), "own pid");
        assert!(marker.exists(), "a live marker stays");
        let mut child = std::process::Command::new("true").spawn().unwrap();
        child.wait().unwrap();
        std::fs::write(&marker, child.id().to_string()).unwrap();
        assert!(!live_watch(home, "S"), "reaped pid");
        assert!(!marker.exists(), "the stale marker is removed");
        // Another session's marker is not this session's.
        std::fs::write(marker_path(home, "T"), std::process::id().to_string()).unwrap();
        assert!(!live_watch(home, "S"));
    }

    #[test]
    fn follow_interval_reads_the_env_var_with_a_five_second_default() {
        use std::ffi::OsStr;
        let secs = |s: &str| follow_interval(Some(OsStr::new(s)));
        assert_eq!(follow_interval(None), Duration::from_secs(5));
        assert_eq!(secs("0.05"), Duration::from_millis(50));
        assert_eq!(secs("2"), Duration::from_secs(2));
        assert_eq!(secs("0"), Duration::ZERO);
        for bad in ["", "x", "-1", "inf", "nan"] {
            assert_eq!(secs(bad), Duration::from_secs(5), "{bad:?}");
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
