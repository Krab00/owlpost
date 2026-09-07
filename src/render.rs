//! Markdown rendering of peer messages for `--format claude|codex|kimi` (OWL-032, §9): the
//! framed block `owl show <id> --format claude` prints, the answers table
//! `owl inbox --format claude` prints, and the per-peer colour markers persisted in
//! `$OWLPOST_HOME/markers.json`. The CLI renders, the model pastes — no template lives in
//! prose. Free of CLI-only types so the daemon can write the same block into a wake file.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context;

use crate::envelope::{self, Payload};
use crate::spool::{Dir, Spool};

/// The top and bottom line of a framed message: exactly 16 × 🟧.
pub const FRAME: &str = "🟧🟧🟧🟧🟧🟧🟧🟧🟧🟧🟧🟧🟧🟧🟧🟧";

/// Per-peer markers, assigned in order of first appearance, then repeating.
pub const MARKERS: [&str; 6] = ["🟦", "🟩", "🟨", "🟪", "🟧", "🟥"];

/// `$OWLPOST_HOME/markers.json`: fingerprint → marker index, append-only.
pub const MARKERS_FILE: &str = "markers.json";

/// Seconds an answer stays in the table.
pub const TABLE_WINDOW_SECS: u64 = 86_400;
/// Rows the table shows at most (the newest).
pub const TABLE_ROWS: usize = 10;

/// The line under the table when rows were cut.
pub fn older_line(n: usize) -> String {
    format!("{n} older answers not shown — owl history")
}

/// The note under a draft whose language differs from the question's.
pub const LANGUAGE_NOTE: &str =
    "note: the draft is in a different language than the question — pick Edit";

/// A ```` ```text ```` block around `text`, verbatim. The fence is one backtick longer than
/// the longest backtick run inside the text (three at least), so a text containing three
/// backticks is fenced with four. Trailing line breaks are dropped.
pub fn text_block(text: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    let body = text.trim_end_matches(['\r', '\n']);
    format!("{fence}text\n{body}\n{fence}")
}

/// [`FRAME`], `header`, the text block, [`FRAME`].
pub fn frame(header: &str, text: &str) -> String {
    format!("{FRAME}\n{header}\n{}\n{FRAME}", text_block(text))
}

/// `🦉 **<name>** · HH:MM · <project or "-"> · <path or "whole repository">`; with a
/// fingerprint (a `consent` record) ` (<fingerprint>)` follows the bold name.
pub fn header(
    peer_name: &str,
    fingerprint: Option<&str>,
    hh_mm: &str,
    project: &str,
    path: Option<&str>,
) -> String {
    let fp = fingerprint.map_or(String::new(), |f| format!(" ({f})"));
    let project = if project.is_empty() { "-" } else { project };
    format!(
        "🦉 **{peer_name}**{fp} · {hh_mm} · {project} · {}",
        path.unwrap_or("whole repository")
    )
}

/// Local wall-clock `HH:MM` of an RFC 3339 timestamp; `--:--` when it does not parse.
pub fn local_hh_mm(rfc3339: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(rfc3339).map_or_else(
        |_| "--:--".to_string(),
        |t| t.with_timezone(&chrono::Local).format("%H:%M").to_string(),
    )
}

/// `draft:` and the draft in a plain text block — drafts are ours, never framed.
pub fn draft_block(text: &str) -> String {
    format!("draft:\n{}", text_block(text))
}

// ponytail: Polish/English stopwords only — swap for a real detector when a third language
// shows up.
const POLISH: &[&str] = &[
    "i", "w", "nie", "na", "jest", "się", "to", "że", "jak", "czy", "z", "do", "ale", "tak", "co",
    "o", "od", "po", "dla", "przez", "tego", "jaki", "masz", "u", "siebie",
];
const ENGLISH: &[&str] = &[
    "the", "a", "an", "and", "is", "are", "of", "to", "in", "it", "that", "this", "you", "we",
    "not", "for", "on", "with", "as", "be", "have", "do", "does", "what", "which", "your", "my",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Language {
    Polish,
    English,
}

/// The stopword list with more hits; ties and zero hits are unknown.
fn language(text: &str) -> Option<Language> {
    let lower = text.to_lowercase();
    let words = lower.split(|c: char| !c.is_alphanumeric());
    let (mut pl, mut en) = (0usize, 0usize);
    for w in words.filter(|w| !w.is_empty()) {
        if POLISH.contains(&w) {
            pl += 1;
        }
        if ENGLISH.contains(&w) {
            en += 1;
        }
    }
    match pl.cmp(&en) {
        std::cmp::Ordering::Greater => Some(Language::Polish),
        std::cmp::Ordering::Less => Some(Language::English),
        std::cmp::Ordering::Equal => None,
    }
}

/// [`LANGUAGE_NOTE`] when the question and the draft are detected as different languages;
/// nothing when either is unknown or both agree.
pub fn language_note(question: &str, draft: &str) -> Option<&'static str> {
    match (language(question), language(draft)) {
        (Some(q), Some(d)) if q != d => Some(LANGUAGE_NOTE),
        _ => None,
    }
}

/// A table cell: `|` escaped as `\|`, line breaks (`\r\n` or `\n`) as `<br>`; trailing
/// line breaks dropped.
pub fn cell(text: &str) -> String {
    text.trim_end_matches(['\r', '\n'])
        .replace('|', "\\|")
        .replace("\r\n", "<br>")
        .replace('\n', "<br>")
}

/// `| <marker> HH:MM · <name> | <text> |`.
pub fn table_row(marker: &str, hh_mm: &str, peer_name: &str, text: &str) -> String {
    format!("| {marker} {hh_mm} · {peer_name} | {} |", cell(text))
}

/// Fingerprint → marker index, loaded from and appended to `$OWLPOST_HOME/markers.json`, so
/// the same peer keeps the same marker across calls and sessions.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Markers {
    map: BTreeMap<String, usize>,
    /// Appearance order, so the index a peer gets is the number of peers seen before it.
    order: Vec<String>,
    dirty: bool,
}

impl Markers {
    /// A missing or unparseable file is an empty map.
    pub fn load(home: &Path) -> Markers {
        let map: BTreeMap<String, usize> = std::fs::read(home.join(MARKERS_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let mut order: Vec<(usize, String)> = map.iter().map(|(k, v)| (*v, k.clone())).collect();
        order.sort();
        Markers {
            order: order.into_iter().map(|(_, k)| k).collect(),
            map,
            dirty: false,
        }
    }

    /// The marker of `fingerprint`; a first-time peer gets the next one (`count % 6`).
    pub fn marker_for(&mut self, fingerprint: &str) -> &'static str {
        let idx = match self.map.get(fingerprint) {
            Some(i) => *i,
            None => {
                let i = self.order.len();
                self.map.insert(fingerprint.to_string(), i);
                self.order.push(fingerprint.to_string());
                self.dirty = true;
                i
            }
        };
        MARKERS[idx % MARKERS.len()]
    }

    /// Writes the map atomically (`markers.json.tmp` + rename) when a marker was assigned.
    pub fn save(&self, home: &Path) -> anyhow::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let path = home.join(MARKERS_FILE);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&self.map)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        if let Err(e) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e).with_context(|| format!("renaming to {}", path.display()));
        }
        Ok(())
    }
}

/// One received answer, as the table needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswerRow {
    pub received_at: String,
    pub fingerprint: String,
    pub peer_name: String,
    pub answer: String,
    /// The question's id (`in_reply_to`), when known.
    pub question_id: Option<String>,
    /// The first line of that question, or empty when the question is not in the spool.
    pub question_first_line: String,
}

/// The last 8 characters of an id (the random tail of a UUIDv7; its head is the timestamp).
pub fn short_id(id: &str) -> String {
    let n = id.chars().count();
    id.chars().skip(n.saturating_sub(8)).collect()
}

/// The answers table: rows received within the last [`TABLE_WINDOW_SECS`], newest last, the
/// [`TABLE_ROWS`] newest of them; above it one `<marker> ↳ <question id short> "<first
/// line, ≤60 chars>"` line per shown row; under it [`older_line`] when any row was cut
/// (older than the window or beyond the row cap). Markers are assigned in order of first
/// appearance in the shown rows. Empty string for no rows.
pub fn answers_table(rows: &[AnswerRow], now_unix: u64, markers: &mut Markers) -> String {
    let mut in_window: Vec<(u64, &AnswerRow)> = rows
        .iter()
        .filter_map(|r| {
            let t = envelope::parse_rfc3339_to_unix(&r.received_at)?;
            (now_unix.saturating_sub(t) <= TABLE_WINDOW_SECS).then_some((t, r))
        })
        .collect();
    in_window.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.received_at.cmp(&b.1.received_at))
    });
    let shown: Vec<&AnswerRow> = in_window
        .iter()
        .skip(in_window.len().saturating_sub(TABLE_ROWS))
        .map(|(_, r)| *r)
        .collect();
    if shown.is_empty() {
        return String::new();
    }
    let cut = rows.len() - shown.len();
    let mut refs = Vec::new();
    let mut lines = Vec::new();
    for r in &shown {
        let marker = markers.marker_for(&r.fingerprint);
        let first: String = r
            .question_first_line
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(60)
            .collect();
        refs.push(format!(
            "{marker} ↳ {} \"{first}\"",
            r.question_id.as_deref().map_or("-".to_string(), short_id)
        ));
        lines.push(table_row(
            marker,
            &local_hh_mm(&r.received_at),
            &r.peer_name,
            &r.answer,
        ));
    }
    let mut out = refs;
    out.push(lines[0].clone());
    out.push("|---|---|".to_string());
    out.extend(lines.into_iter().skip(1));
    if cut > 0 {
        out.push(older_line(cut));
    }
    out.join("\n")
}

/// The question an answer replies to: `done/` (an answered ask), `asks/` (still open) or
/// `inbox/` (a question received here), whichever holds `id`.
pub fn find_question(spool: &Spool, id: &str) -> Option<Payload> {
    [Dir::Done, Dir::Asks, Dir::Inbox]
        .into_iter()
        .find_map(|d| spool.get(d, id).ok().flatten())
        .and_then(|rec| serde_json::from_str(&rec.raw).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(received_at: &str, fp: &str, name: &str, answer: &str, q: &str) -> AnswerRow {
        AnswerRow {
            received_at: received_at.into(),
            fingerprint: fp.into(),
            peer_name: name.into(),
            answer: answer.into(),
            question_id: Some(format!("0192aaaa-bbbb-7ccc-8ddd-{q:0>12}")),
            question_first_line: format!("question {q}\nsecond line"),
        }
    }

    #[test]
    fn frame_is_sixteen_icons_header_and_fence() {
        assert_eq!(FRAME.chars().count(), 16);
        assert!(FRAME.chars().all(|c| c == '🟧'));
        let h = header("Ana", None, "09:08", "github.com/x/y", None);
        assert_eq!(h, "🦉 **Ana** · 09:08 · github.com/x/y · whole repository");
        let f = frame(&h, "Jaki masz ostatni commit?");
        assert_eq!(
            f,
            format!("{FRAME}\n{h}\n```text\nJaki masz ostatni commit?\n```\n{FRAME}")
        );
        assert_eq!(
            header("Ana", Some("owl:abc"), "09:08", "p", Some("src/a.rs")),
            "🦉 **Ana** (owl:abc) · 09:08 · p · src/a.rs"
        );
        assert_eq!(
            header("Ana", None, "09:08", "", None),
            "🦉 **Ana** · 09:08 · - · whole repository"
        );
    }

    #[test]
    fn fence_grows_past_backticks_in_the_text() {
        assert_eq!(text_block("plain"), "```text\nplain\n```");
        assert_eq!(
            text_block("has ``` inside"),
            "````text\nhas ``` inside\n````"
        );
        assert_eq!(
            text_block("has ```` inside"),
            "`````text\nhas ```` inside\n`````"
        );
        assert_eq!(text_block("one ` tick"), "```text\none ` tick\n```");
        assert_eq!(text_block("trailing\n\n"), "```text\ntrailing\n```");
        assert_eq!(draft_block("d"), "draft:\n```text\nd\n```");
    }

    #[test]
    fn hh_mm_handles_garbage() {
        assert_eq!(local_hh_mm("garbage"), "--:--");
        let t = local_hh_mm("2026-09-06T23:08:11Z");
        assert_eq!(t.len(), 5, "{t}");
        assert_eq!(&t[2..3], ":");
    }

    #[test]
    fn language_note_only_when_both_known_and_different() {
        let pl = "Jaki masz ostatni commit u siebie i czy to jest ok?";
        let en = "The last commit is on main and it is fine, what do you need?";
        let pl2 = "Nie, to nie jest tak jak myślisz, ale się zgadzam.";
        let en2 = "This is the file you are looking for.";
        assert_eq!(language_note(pl, en), Some(LANGUAGE_NOTE));
        assert_eq!(language_note(en, pl), Some(LANGUAGE_NOTE));
        assert_eq!(language_note(pl, pl2), None);
        assert_eq!(language_note(en, en2), None);
        assert_eq!(language_note("12345 ???", en), None, "unknown question");
        assert_eq!(language_note(pl, "src/main.rs:42"), None, "unknown draft");
        assert_eq!(language_note("", ""), None);
        // `to`, `do`, `o` are in both lists: a tie is unknown.
        assert_eq!(language("to do"), None);
    }

    #[test]
    fn cell_escapes_pipes_and_breaks() {
        assert_eq!(cell("a|b"), "a\\|b");
        assert_eq!(cell("l1\nl2\r\nl3\n"), "l1<br>l2<br>l3");
        assert_eq!(
            table_row("🟦", "00:16", "Ana", "x|y\nz"),
            "| 🟦 00:16 · Ana | x\\|y<br>z |"
        );
    }

    #[test]
    fn markers_follow_first_appearance_and_persist() {
        let home = tempfile::tempdir().unwrap();
        let mut m = Markers::load(home.path());
        assert_eq!(m.marker_for("owl:b"), "🟦");
        assert_eq!(m.marker_for("owl:a"), "🟩");
        assert_eq!(m.marker_for("owl:b"), "🟦");
        for (i, fp) in ["c", "d", "e", "f", "g"].iter().enumerate() {
            assert_eq!(m.marker_for(fp), MARKERS[(i + 2) % 6]);
        }
        assert_eq!(m.marker_for("g"), "🟦", "wraps after six");
        m.save(home.path()).unwrap();
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(home.path().join(MARKERS_FILE)).unwrap())
                .unwrap();
        assert_eq!(v["owl:b"], 0);
        assert_eq!(v["owl:a"], 1);
        assert_eq!(v["g"], 6);
        let mut again = Markers::load(home.path());
        assert_eq!(again.marker_for("owl:a"), "🟩");
        assert_eq!(again.marker_for("owl:b"), "🟦");
        assert_eq!(again.marker_for("new"), MARKERS[7 % 6]);
        // A missing or broken file is an empty map; nothing is written until a marker is new.
        let empty = tempfile::tempdir().unwrap();
        std::fs::write(empty.path().join(MARKERS_FILE), "{").unwrap();
        let m = Markers::load(empty.path());
        assert_eq!(m, Markers::default());
        m.save(empty.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(empty.path().join(MARKERS_FILE)).unwrap(),
            "{"
        );
    }

    #[test]
    fn table_keeps_24h_window_and_10_newest_rows_newest_last() {
        let now = envelope::parse_rfc3339_to_unix("2026-09-07T12:00:00Z").unwrap();
        // 12 answers over 26 h, one per 2 h, alternating peers: 11 within 24 h, 1 older.
        let rows: Vec<AnswerRow> = (0..12)
            .map(|i| {
                let t = envelope::unix_to_rfc3339(now - 26 * 3600 + i * 2 * 3600 + 1);
                let (fp, name) = if i % 2 == 0 {
                    ("owl:one", "One")
                } else {
                    ("owl:two", "Two")
                };
                row(&t, fp, name, &format!("answer {i}"), &i.to_string())
            })
            .collect();
        // Handed over out of order: the table sorts by `received_at`, not by input order.
        let mut shuffled = rows.clone();
        shuffled.rotate_left(5);
        shuffled.swap(0, 3);
        assert_ne!(shuffled, rows);
        let mut m = Markers::default();
        let out = answers_table(&shuffled, now, &mut m);
        let lines: Vec<&str> = out.lines().collect();
        let refs: Vec<&&str> = lines.iter().filter(|l| l.contains(" ↳ ")).collect();
        let table: Vec<&&str> = lines.iter().filter(|l| l.starts_with("| ")).collect();
        assert_eq!(refs.len(), 10);
        assert_eq!(table.len(), 10);
        assert_eq!(lines[10 + 1], "|---|---|");
        assert_eq!(lines.last().unwrap(), &older_line(2));
        // Rows 0 (older than 24 h) and 1 (oldest in window) are cut; 2..12 shown, ascending.
        assert!(table[0].ends_with("| answer 2 |"), "{}", table[0]);
        assert!(table[9].ends_with("| answer 11 |"), "{}", table[9]);
        assert!(!out.contains("answer 0 ") && !out.contains("answer 1 "));
        // Row 2 is from peer one: the first-appearing peer gets 🟦, the other 🟩.
        assert!(table[0].starts_with("| 🟦 "), "{}", table[0]);
        assert!(table[1].starts_with("| 🟩 "), "{}", table[1]);
        assert!(
            refs[0].starts_with("🟦 ↳ 00000002 \"question 2\""),
            "{}",
            refs[0]
        );
        // Exactly at the window edge stays; one second past it goes.
        let edge = row(
            &envelope::unix_to_rfc3339(now - 86_400),
            "x",
            "X",
            "edge",
            "e",
        );
        let past = row(
            &envelope::unix_to_rfc3339(now - 86_401),
            "x",
            "X",
            "past",
            "p",
        );
        let out = answers_table(&[past, edge], now, &mut Markers::default());
        assert!(
            out.contains("| edge |") && !out.contains("| past |"),
            "{out}"
        );
        assert!(out.ends_with(&older_line(1)), "{out}");
        assert_eq!(answers_table(&[], now, &mut Markers::default()), "");
    }

    #[test]
    fn table_names_missing_questions_with_an_empty_string() {
        let now = envelope::parse_rfc3339_to_unix("2026-09-07T12:00:00Z").unwrap();
        let mut r = row(&envelope::unix_to_rfc3339(now - 5), "a", "A", "x", "1");
        r.question_first_line = String::new();
        let out = answers_table(&[r.clone()], now, &mut Markers::default());
        assert!(out.starts_with("🟦 ↳ 00000001 \"\"\n| 🟦 "), "{out}");
        r.question_id = None;
        let out = answers_table(&[r], now, &mut Markers::default());
        assert!(out.starts_with("🟦 ↳ - \"\"\n"), "{out}");
        assert_eq!(short_id("0192aaaa-bbbb-7ccc-8ddd-000000000001"), "00000001");
        assert_eq!(short_id("abc"), "abc");
    }

    #[test]
    fn reference_line_keeps_the_first_60_chars_of_the_first_line() {
        let now = envelope::parse_rfc3339_to_unix("2026-09-07T12:00:00Z").unwrap();
        let seventy = "x".repeat(70);
        let sixty = "y".repeat(60);
        let mut long = row(&envelope::unix_to_rfc3339(now - 5), "a", "A", "x", "1");
        long.question_first_line = format!("{seventy}\nsecond line");
        let mut exact = row(&envelope::unix_to_rfc3339(now - 4), "a", "A", "y", "2");
        exact.question_first_line = sixty.clone();
        let out = answers_table(&[long, exact], now, &mut Markers::default());
        let refs: Vec<&str> = out.lines().filter(|l| l.contains(" ↳ ")).collect();
        assert_eq!(refs[0], format!("🟦 ↳ 00000001 \"{}\"", "x".repeat(60)));
        assert_eq!(refs[1], format!("🟦 ↳ 00000002 \"{sixty}\""));
        assert!(!out.contains(&"x".repeat(61)));
    }
}
