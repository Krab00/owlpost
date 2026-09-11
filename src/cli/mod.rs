//! Subcommand implementations (ask; inbox side, design §8–§9; install/watch/doctor), the
//! shared exit-code error type, and the helpers the inbox commands share: record lookup,
//! payload parsing, the stored draft shape, peer naming, age formatting and machine output.

pub mod add;
pub mod allow;
pub mod ask;
pub mod deny;
pub mod doctor;
pub mod draft;
pub mod edit;
pub mod history;
pub mod inbox;
pub mod install;
pub mod mcp;
pub mod reject;
pub mod route;
pub mod send;
pub mod setup;
pub mod show;
pub mod update;
pub mod watch;

use std::path::Path;

use anyhow::Context;
use owlpost::contacts::ContactBook;
use owlpost::envelope::{self, Body, Kind, Payload};
use owlpost::spool::Record;
use serde_json::{Value, json};

/// An error carrying its §9 process exit code (`1` user/data, `2` offline/unavailable, `3`
/// rate limited, `4` nothing to do, e.g. a `watch` or `ask --wait` timeout). `main` downcasts
/// it via [`exit_code`]; every other error exits `1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitError {
    pub code: u8,
    pub message: String,
}

impl ExitError {
    pub fn new(code: u8, message: impl Into<String>) -> ExitError {
        ExitError {
            code,
            message: message.into(),
        }
    }

    /// `new`, already wrapped as an `anyhow::Error` for `?`-style returns.
    pub fn error(code: u8, message: impl Into<String>) -> anyhow::Error {
        ExitError::new(code, message).into()
    }
}

impl std::fmt::Display for ExitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ExitError {}

/// Exit code for a failed command: the `ExitError` code when there is one, else `1`.
pub fn exit_code(e: &anyhow::Error) -> u8 {
    e.downcast_ref::<ExitError>().map_or(1, |x| x.code)
}

pub use owlpost::answer::{StoredDraft, existing_answer, finish, inbox_record, payload_of};

/// Loads the merged contact book for the current directory; a missing git root is fine.
pub fn contact_book(home: &Path) -> anyhow::Result<ContactBook> {
    let cwd = std::env::current_dir().context("reading the current directory")?;
    ContactBook::load(home, &cwd)
}

/// Contact name for a fingerprint, or the fingerprint itself for unknown peers.
pub fn peer_name(book: &ContactBook, fp: &str) -> String {
    owlpost::render::peer_name(book, fp)
}

/// Seconds between `received_at` and now (0 when unparsable or in the future).
pub fn age_secs(received_at: &str, now: u64) -> u64 {
    envelope::parse_rfc3339_to_unix(received_at).map_or(0, |t| now.saturating_sub(t))
}

/// `12s`, `5m`, `3h`, `2d`.
pub fn format_age(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s => format!("{}d", s / 86_400),
    }
}

pub fn kind_str(kind: Kind) -> &'static str {
    match kind {
        Kind::Question => "question",
        Kind::Answer => "answer",
    }
}

/// The file path a record is about: the question's `path`; answers and repo-level questions
/// carry none, so `-`.
pub fn body_path(body: &Body) -> &str {
    match body {
        Body::Question {
            path: Some(path), ..
        } => path,
        Body::Question { path: None, .. } | Body::Answer { .. } => "-",
    }
}

/// One listing row shared by `inbox` and `history`: the fields §9 asks for plus the raw
/// timestamps for machine consumers.
pub fn summary(id: &str, rec: &Record, payload: &Payload, book: &ContactBook, now: u64) -> Value {
    let age = age_secs(&rec.received_at, now);
    json!({
        "id": id,
        "from": payload.from,
        "from_name": peer_name(book, &payload.from),
        "to": payload.to,
        "type": kind_str(payload.kind),
        "state": rec.state,
        "seen": rec.seen,
        "path": body_path(&payload.body),
        "project": match &payload.body { Body::Question { project, .. } => Some(project.as_str()), Body::Answer { .. } => None },
        "received_at": rec.received_at,
        "age_secs": age,
        "age": format_age(age),
        "has_draft": rec.draft.as_ref().is_some_and(|d| !d.is_null()),
    })
}

/// ANSI colour per row when stdout is a terminal and `NO_COLOR` is unset: questions yellow,
/// answers green, unseen rows bold. Empty string = no colour.
pub fn row_style(kind: &str, seen: bool) -> &'static str {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() || std::env::var_os("NO_COLOR").is_some() {
        return "";
    }
    match (kind, seen) {
        ("question", false) => "\x1b[1;33m",
        ("question", true) => "\x1b[33m",
        ("answer", false) => "\x1b[1;32m",
        ("answer", true) => "\x1b[32m",
        _ => "",
    }
}

/// A plain-text table: header from `cols`, rows from `rows`, each column padded to its widest
/// cell. Nothing is printed for an empty `rows`.
pub fn print_table(cols: &[&str], rows: &[Vec<String>]) {
    print_table_styled(cols, rows, &[]);
}

/// `print_table` with an optional ANSI prefix per row (see [`row_style`]); padding is computed
/// on the plain cells so escapes never shift columns.
pub fn print_table_styled(cols: &[&str], rows: &[Vec<String>], styles: &[&str]) {
    if rows.is_empty() {
        return;
    }
    let widths: Vec<usize> = (0..cols.len())
        .map(|i| {
            rows.iter()
                .map(|r| r[i].len())
                .chain(std::iter::once(cols[i].len()))
                .max()
                .unwrap_or(0)
        })
        .collect();
    let line = |cells: Vec<&str>, style: &str| {
        let mut s = String::new();
        for (i, c) in cells.iter().enumerate() {
            if i + 1 == cells.len() {
                s.push_str(c);
            } else {
                s.push_str(&format!("{:<w$}  ", c, w = widths[i]));
            }
        }
        if style.is_empty() {
            println!("{}", s.trim_end());
        } else {
            println!("{style}{}\x1b[0m", s.trim_end());
        }
    };
    line(cols.to_vec(), "");
    for (i, r) in rows.iter().enumerate() {
        line(
            r.iter().map(String::as_str).collect(),
            styles.get(i).copied().unwrap_or(""),
        );
    }
}

pub fn print_json(v: &Value) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

pub fn user_error(message: impl Into<String>) -> anyhow::Error {
    ExitError::new(1, message).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_formats_each_unit() {
        assert_eq!(format_age(0), "0s");
        assert_eq!(format_age(59), "59s");
        assert_eq!(format_age(60), "1m");
        assert_eq!(format_age(3_599), "59m");
        assert_eq!(format_age(3_600), "1h");
        assert_eq!(format_age(86_399), "23h");
        assert_eq!(format_age(86_400), "1d");
        assert_eq!(format_age(200_000), "2d");
    }

    #[test]
    fn age_secs_handles_future_and_garbage() {
        assert_eq!(age_secs("2026-09-01T10:00:00Z", 1_787_000_000), 0);
        assert_eq!(age_secs("garbage", 1_000), 0);
        let t = envelope::parse_rfc3339_to_unix("2026-09-01T10:00:00Z").unwrap();
        assert_eq!(age_secs("2026-09-01T10:00:00Z", t + 90), 90);
    }

    #[test]
    fn exit_error_displays_message_and_downcasts() {
        let e: anyhow::Error = ExitError::new(4, "timeout").into();
        assert_eq!(e.to_string(), "timeout");
        assert_eq!(e.downcast_ref::<ExitError>().map(|x| x.code), Some(4));
        assert_eq!(exit_code(&e), 4);
        let plain = anyhow::anyhow!("boom");
        assert!(plain.downcast_ref::<ExitError>().is_none());
        assert_eq!(exit_code(&plain), 1);
        let wrapped = ExitError::error(3, "rate limited");
        assert_eq!(wrapped.to_string(), "rate limited");
        assert_eq!(exit_code(&wrapped), 3);
    }

    #[test]
    fn exit_error_displays_message_only() {
        let e = ExitError {
            code: 4,
            message: "nothing".into(),
        };
        assert_eq!(e.to_string(), "nothing");
        let any: anyhow::Error = e.into();
        assert_eq!(any.downcast_ref::<ExitError>().unwrap().code, 4);
    }

    /// `main` exits with the code an `ExitError` carries (§9: 2 offline, 3 rate limited,
    /// 4 nothing to do) and with 1 for every other error, including a wrapped one.
    #[test]
    fn exit_code_comes_from_exit_error_else_1() {
        for code in [1u8, 2, 3, 4] {
            let e: anyhow::Error = ExitError {
                code,
                message: "x".into(),
            }
            .into();
            assert_eq!(exit_code(&e), code);
            assert_eq!(
                exit_code(&e.context("wrapped")),
                code,
                "context keeps the code"
            );
        }
        assert_eq!(exit_code(&anyhow::anyhow!("plain")), 1);
        assert_eq!(exit_code(&user_error("user")), 1);
    }
}
