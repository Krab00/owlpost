//! Inbox-side subcommands (design §8–§9) and the helpers they share: record lookup, payload
//! parsing, the stored draft shape, peer naming, age formatting and machine output.

pub mod draft;
pub mod edit;
pub mod history;
pub mod inbox;
pub mod reject;
pub mod send;
pub mod show;

use std::path::Path;

use anyhow::Context;
use owlpost::contacts::ContactBook;
use owlpost::envelope::{self, Body, Kind, Payload};
use owlpost::spool::{Dir, Record, Spool};
use serde_json::{Map, Value, json};

/// An error that carries its own process exit code (§9: 1 user/data, 2 offline, 3 rate
/// limited, 4 nothing to do). `main` downcasts it; every other error exits 1.
#[derive(Debug)]
pub struct ExitError {
    pub code: u8,
    pub message: String,
}

impl std::fmt::Display for ExitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ExitError {}

/// Process exit code for a command error: an `ExitError` carries its own, anything else is 1.
pub fn exit_code(e: &anyhow::Error) -> u8 {
    e.downcast_ref::<ExitError>().map_or(1, |x| x.code)
}

/// Draft as stored on an inbox record (`record.draft`), §3.4 step 4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredDraft {
    pub text: String,
    pub harness: String,
    pub redactions: u32,
    pub status: String,
    pub drafted_at: String,
}

impl StoredDraft {
    pub fn from_runner(d: &owlpost::runner::Draft) -> StoredDraft {
        StoredDraft {
            text: d.text.clone(),
            harness: d.harness.clone(),
            redactions: u32::try_from(d.redactions).unwrap_or(u32::MAX),
            status: d.status.as_str().to_string(),
            drafted_at: envelope::rfc3339_now(),
        }
    }

    pub fn to_value(&self) -> Value {
        json!({
            "text": self.text,
            "harness": self.harness,
            "redactions": self.redactions,
            "status": self.status,
            "drafted_at": self.drafted_at,
        })
    }

    /// `None` when the record has no draft; an error when it has one of the wrong shape.
    pub fn from_record(id: &str, rec: &Record) -> anyhow::Result<Option<StoredDraft>> {
        let Some(v) = rec.draft.as_ref().filter(|v| !v.is_null()) else {
            return Ok(None);
        };
        let field = |name: &str| -> anyhow::Result<&str> {
            v.get(name)
                .and_then(Value::as_str)
                .with_context(|| format!("record {id}: draft is malformed (missing {name})"))
        };
        let redactions = v
            .get("redactions")
            .and_then(Value::as_u64)
            .with_context(|| format!("record {id}: draft is malformed (missing redactions)"))?;
        Ok(Some(StoredDraft {
            text: field("text")?.to_string(),
            harness: field("harness")?.to_string(),
            redactions: u32::try_from(redactions).unwrap_or(u32::MAX),
            status: field("status")?.to_string(),
            drafted_at: field("drafted_at")?.to_string(),
        }))
    }
}

/// Parses the stored raw payload. The bytes were verified on receipt (§3.2), so no
/// signature check here.
pub fn payload_of(id: &str, rec: &Record) -> anyhow::Result<Payload> {
    serde_json::from_str(&rec.raw).with_context(|| format!("record {id}: payload is malformed"))
}

/// The inbox record for `id`, or a user error naming the id.
pub fn inbox_record(spool: &Spool, id: &str) -> anyhow::Result<Record> {
    spool
        .get(Dir::Inbox, id)?
        .with_context(|| format!("no inbox record {id}"))
}

/// `rec.meta` as an object. Records carry an object there (`{peer, hash}` from the daemon), but
/// the value is stored JSON, so a stray scalar or array is replaced by an empty object rather
/// than indexed into (OWL-004 lesson: never `IndexMut` a parsed `Value`).
pub fn meta_object(rec: &mut Record) -> &mut Map<String, Value> {
    if !rec.meta.is_object() {
        rec.meta = Value::Object(Map::new());
    }
    match &mut rec.meta {
        Value::Object(m) => m,
        _ => unreachable!("meta was just set to an object"),
    }
}

/// Finishes an inbox record: `state`, `meta.done_at` and the `extra` meta keys are set on the
/// record, which is written atomically (temp file + rename, `Spool::put`) into `done/`; only
/// after that succeeds is the `inbox/` file removed. A failure writing `done/` therefore leaves
/// the inbox record byte-for-byte as it was, so the caller can simply be run again; a failure
/// removing the inbox file leaves a finished copy in `done/` next to the untouched original,
/// which a retry overwrites in place.
pub fn finish(
    spool: &Spool,
    id: &str,
    mut rec: Record,
    state: &str,
    extra: &[(&str, Value)],
) -> anyhow::Result<()> {
    rec.state = state.to_string();
    let meta = meta_object(&mut rec);
    meta.insert("done_at".into(), json!(envelope::rfc3339_now()));
    for (k, v) in extra {
        meta.insert((*k).to_string(), v.clone());
    }
    spool
        .put(Dir::Done, id, &rec)
        .with_context(|| format!("finishing record {id}"))?;
    let src = spool.path(Dir::Inbox, id);
    std::fs::remove_file(&src).with_context(|| format!("removing {}", src.display()))
}

/// Loads the merged contact book for the current directory; a missing git root is fine.
pub fn contact_book(home: &Path) -> anyhow::Result<ContactBook> {
    let cwd = std::env::current_dir().context("reading the current directory")?;
    ContactBook::load(home, &cwd)
}

/// Contact name for a fingerprint, or the fingerprint itself for unknown peers.
pub fn peer_name(book: &ContactBook, fp: &str) -> String {
    book.contacts
        .iter()
        .find(|c| c.fingerprint == fp)
        .map_or_else(|| fp.to_string(), |c| c.name.clone())
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

/// The file path a record is about: the question's `path`; answers carry none, so `-`.
pub fn body_path(body: &Body) -> &str {
    match body {
        Body::Question { path, .. } => path,
        Body::Answer { .. } => "-",
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

/// A plain-text table: header from `cols`, rows from `rows`, each column padded to its widest
/// cell. Nothing is printed for an empty `rows`.
pub fn print_table(cols: &[&str], rows: &[Vec<String>]) {
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
    let line = |cells: Vec<&str>| {
        let mut s = String::new();
        for (i, c) in cells.iter().enumerate() {
            if i + 1 == cells.len() {
                s.push_str(c);
            } else {
                s.push_str(&format!("{:<w$}  ", c, w = widths[i]));
            }
        }
        println!("{}", s.trim_end());
    };
    line(cols.to_vec());
    for r in rows {
        line(r.iter().map(String::as_str).collect());
    }
}

pub fn print_json(v: &Value) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

pub fn user_error(message: impl Into<String>) -> anyhow::Error {
    ExitError {
        code: 1,
        message: message.into(),
    }
    .into()
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

    fn rec(draft: Value) -> Record {
        Record {
            raw: String::new(),
            sig: String::new(),
            state: "drafted".into(),
            seen: false,
            received_at: "2026-09-01T10:00:00Z".into(),
            draft: Some(draft),
            meta: Value::Null,
        }
    }

    #[test]
    fn stored_draft_round_trips_and_rejects_malformed() {
        let d = StoredDraft {
            text: "t".into(),
            harness: "fake".into(),
            redactions: 1,
            status: "ok".into(),
            drafted_at: "2026-09-01T10:00:00Z".into(),
        };
        let back = StoredDraft::from_record("x", &rec(d.to_value()))
            .unwrap()
            .unwrap();
        assert_eq!(back, d);
        assert!(
            StoredDraft::from_record("x", &rec(Value::Null))
                .unwrap()
                .is_none()
        );
        let mut r = rec(Value::Null);
        r.draft = None;
        assert!(StoredDraft::from_record("x", &r).unwrap().is_none());
        for bad in [
            json!({"harness": "fake", "redactions": 0, "status": "ok", "drafted_at": "x"}),
            json!({"text": "t", "harness": "fake", "redactions": "1", "status": "ok", "drafted_at": "x"}),
            json!({"text": 5, "harness": "fake", "redactions": 0, "status": "ok", "drafted_at": "x"}),
            json!("just a string"),
            json!([]),
        ] {
            let err = StoredDraft::from_record("x", &rec(bad.clone()))
                .unwrap_err()
                .to_string();
            assert!(err.contains("record x: draft is malformed"), "{bad}: {err}");
        }
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

    /// Every non-object `meta` shape becomes an empty object; an object is kept as is.
    #[test]
    fn meta_object_repairs_non_objects_and_keeps_objects() {
        for bad in [
            Value::Null,
            json!("oops"),
            json!([1]),
            json!(7),
            json!(true),
        ] {
            let mut r = rec(Value::Null);
            r.meta = bad.clone();
            meta_object(&mut r).insert("k".into(), json!(1));
            assert_eq!(r.meta, json!({ "k": 1 }), "{bad}");
        }
        let mut r = rec(Value::Null);
        r.meta = json!({ "peer": "p", "hash": "h" });
        meta_object(&mut r).insert("k".into(), json!(1));
        assert_eq!(r.meta, json!({ "peer": "p", "hash": "h", "k": 1 }));
    }
}
