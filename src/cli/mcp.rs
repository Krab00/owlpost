//! `owl mcp`: the contact book as MCP resources over stdio (design §9), so Claude Code lists
//! every contact in its `@` typeahead as `@owl:to://<name-slug>.<email>` and attaches the
//! contact (name, fingerprint, e-mails) to the prompt. JSON-RPC 2.0, one object per line;
//! resources only — no tools, no prompts — so the server costs nothing in the model context.
//! The book is re-read from disk on every list/read, so a contact added mid-session shows up.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::Path;

use owlpost::contacts::{Contact, ContactBook};
use serde_json::{Value, json};

/// Protocol version offered when the client names none.
const PROTOCOL_VERSION: &str = "2025-06-18";
const MIME: &str = "application/json";

pub fn run(home: &Path) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = handle_line(home, &cwd, &line) {
            serde_json::to_writer(&mut stdout, &reply)?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

/// The reply for one input line, or `None` for a notification (a request without `id`).
fn handle_line(home: &Path, cwd: &Path, line: &str) -> Option<Value> {
    let req: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return Some(error(Value::Null, -32700, format!("parse error: {e}"))),
    };
    let Some(method) = req.get("method").and_then(Value::as_str) else {
        return Some(error(Value::Null, -32600, "invalid request: no method"));
    };
    let id = req.get("id").filter(|id| !id.is_null()).cloned();
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    let outcome = dispatch(home, cwd, method, &params);
    let id = id?;
    Some(match outcome {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err((code, message)) => error(id, code, message),
    })
}

fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

fn dispatch(home: &Path, cwd: &Path, method: &str, params: &Value) -> Result<Value, (i64, String)> {
    match method {
        "initialize" => {
            let version = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(PROTOCOL_VERSION);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": {"resources": {}},
                "serverInfo": {"name": "owl", "version": env!("CARGO_PKG_VERSION")},
            }))
        }
        "ping" => Ok(json!({})),
        "resources/list" => {
            let book = load(home, cwd)?;
            let resources: Vec<Value> = resources(&book)
                .into_iter()
                .map(|(uri, c)| {
                    json!({
                        "uri": uri,
                        "name": c.name,
                        "description": format!("{} · {}", c.fingerprint, c.source),
                        "mimeType": MIME,
                    })
                })
                .collect();
            Ok(json!({"resources": resources}))
        }
        "resources/read" => {
            let uri = params
                .get("uri")
                .and_then(Value::as_str)
                .ok_or_else(|| (-32602, "invalid params: uri is required".to_string()))?;
            let book = load(home, cwd)?;
            let (_, c) = resources(&book)
                .into_iter()
                .find(|(u, _)| u == uri)
                .ok_or_else(|| (-32002, format!("resource not found: {uri}")))?;
            let text = json!({"name": c.name, "fingerprint": c.fingerprint, "emails": c.emails});
            Ok(json!({"contents": [{"uri": uri, "mimeType": MIME, "text": text.to_string()}]}))
        }
        "resources/templates/list" => Ok(json!({"resourceTemplates": []})),
        "tools/list" => Ok(json!({"tools": []})),
        "prompts/list" => Ok(json!({"prompts": []})),
        other => Err((-32601, format!("method not found: {other}"))),
    }
}

fn load(home: &Path, cwd: &Path) -> Result<ContactBook, (i64, String)> {
    ContactBook::load(home, cwd).map_err(|e| (-32603, format!("loading contacts: {e:#}")))
}

/// `(uri, contact)` per contact, sorted by name (then fingerprint) so the list is stable.
/// URI: `to://<slug>.<first e-mail>` (`to://<slug>` without e-mail); when two contacts share
/// a URI each gets `.<fingerprint without owl:>` appended, so every URI is unique.
fn resources(book: &ContactBook) -> Vec<(String, &Contact)> {
    let mut contacts: Vec<&Contact> = book.contacts.iter().collect();
    contacts.sort_by(|a, b| (&a.name, &a.fingerprint).cmp(&(&b.name, &b.fingerprint)));
    let bases: Vec<String> = contacts.iter().map(|c| base_uri(c)).collect();
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for b in &bases {
        *counts.entry(b).or_default() += 1;
    }
    contacts
        .into_iter()
        .zip(bases.iter())
        .map(|(c, base)| {
            let uri = if counts[base.as_str()] > 1 {
                format!("{base}.{}", fp_segment(c))
            } else {
                base.clone()
            };
            (uri, c)
        })
        .collect()
}

fn base_uri(c: &Contact) -> String {
    let mut segments: Vec<String> = Vec::new();
    let slug = slug(&c.name);
    if !slug.is_empty() {
        segments.push(slug);
    }
    if let Some(email) = c.emails.first().filter(|e| !e.is_empty()) {
        segments.push(email.clone());
    }
    if segments.is_empty() {
        // A nameless, e-mail-less contact (a stray policy overlay): the fingerprint alone.
        segments.push(fp_segment(c).to_string());
    }
    format!("to://{}", segments.join("."))
}

fn fp_segment(c: &Contact) -> &str {
    c.fingerprint.strip_prefix("owl:").unwrap_or(&c.fingerprint)
}

/// Name lower-cased, every run of non-alphanumerics (Unicode: `ë` and `ł` stay) as one `-`,
/// no leading or trailing `-`.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for ch in name.to_lowercase().chars() {
        if ch.is_alphanumeric() {
            out.push(ch);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_rules() {
        assert_eq!(slug("Ana Kowalska"), "ana-kowalska");
        assert_eq!(slug("  Zoë O'Brien-Łukasz!! "), "zoë-o-brien-łukasz");
        assert_eq!(slug("--"), "");
        assert_eq!(slug(""), "");
        assert_eq!(slug("a__b  c"), "a-b-c");
    }

    fn contact(name: &str, emails: &[&str], fp: &str) -> Contact {
        Contact {
            name: name.into(),
            emails: emails.iter().map(|e| e.to_string()).collect(),
            pubkey: String::new(),
            endpoints: vec![],
            source: "global".into(),
            policy: None,
            added_at: None,
            fingerprint: fp.into(),
        }
    }

    #[test]
    fn uris_are_sorted_and_unique() {
        let book = ContactBook {
            contacts: vec![
                contact("Bob", &["bob@x.io"], "owl:bbbb"),
                contact("Ana", &["ana@x.io"], "owl:aaaa"),
                contact("Bob", &["bob@x.io"], "owl:aaab"),
                contact("", &[], "owl:zzzz"),
                contact("No Mail", &[], "owl:nnnn"),
            ],
        };
        let uris: Vec<String> = resources(&book).into_iter().map(|(u, _)| u).collect();
        assert_eq!(
            uris,
            [
                "to://zzzz",
                "to://ana.ana@x.io",
                "to://bob.bob@x.io.aaab",
                "to://bob.bob@x.io.bbbb",
                "to://no-mail",
            ]
        );
    }

    #[test]
    fn notifications_get_no_reply_and_bad_lines_get_null_ids() {
        let dir = tempfile::tempdir().unwrap();
        let (h, c) = (dir.path(), dir.path());
        assert_eq!(
            handle_line(
                h,
                c,
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
            ),
            None
        );
        let bad = handle_line(h, c, "{not json").unwrap();
        assert_eq!(bad["id"], Value::Null);
        assert_eq!(bad["error"]["code"], -32700);
        let not_request = handle_line(h, c, "[1,2]").unwrap();
        assert_eq!(not_request["id"], Value::Null);
        assert_eq!(not_request["error"]["code"], -32600);
        let unknown = handle_line(h, c, r#"{"jsonrpc":"2.0","id":7,"method":"nope"}"#).unwrap();
        assert_eq!(unknown["id"], 7);
        assert_eq!(unknown["error"]["code"], -32601);
        let ping = handle_line(h, c, r#"{"jsonrpc":"2.0","id":"p","method":"ping"}"#).unwrap();
        assert_eq!(ping, json!({"jsonrpc": "2.0", "id": "p", "result": {}}));
    }
}
