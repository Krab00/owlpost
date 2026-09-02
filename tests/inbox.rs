//! OWL-010 inbox CLI tests: every test drives the `owl` binary against its own temp home,
//! writes inbox records straight into the spool (signed by a peer identity) and uses
//! `tests/fixtures/fake-harness.sh` as the responder (design §8, §9, §12).

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use common::{
    PATH, PROJECT, Peer, client, fp, id, policy, post_envelope, prepare_home_with, question,
    respawn, signed,
};
use owlpost::config::Harness;
use owlpost::contacts::Mode;
use owlpost::envelope::{self, Body, Envelope, Kind, Payload};
use owlpost::identity::Identity;
use owlpost::spool::{Dir, Record, Spool};
use serde_json::{Value, json};
use tempfile::TempDir;

const OWL: &str = env!("CARGO_BIN_EXE_owl");
const FAKE_HARNESS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/fake-harness.sh"
);
const CLAUDE_TWO: &str = r#"{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"owlpost: 2 new questions (Maciek 2). Say \"show owlpost inbox\" or run `owl inbox`."}}"#;
const SENTENCE_TWO: &str =
    "owlpost: 2 new questions (Maciek 2). Say \"show owlpost inbox\" or run `owl inbox`.";
const FORMATS: [&str; 4] = ["plain", "claude", "codex", "kimi"];

struct Home {
    dir: TempDir,
    _checkout: TempDir,
    me: Identity,
    maciek: Identity,
    ana: Identity,
    maciej: Identity,
}

impl Home {
    fn new() -> Home {
        Self::with(|_| {})
    }

    fn with(tweak: impl FnOnce(&mut owlpost::config::Config)) -> Home {
        let dir = tempfile::tempdir().unwrap();
        let checkout = tempfile::tempdir().unwrap();
        let (me, maciek, ana, maciej) = (id(2), id(1), id(3), id(5));
        let peers = [
            Peer::new(&maciek, "Maciek", Some(policy(Mode::Manual, None))),
            Peer::new(&ana, "Ana", Some(policy(Mode::Manual, None))),
            // Differs from Maciek in one character: the `--peer` negative twin.
            Peer::new(&maciej, "Maciej", None),
        ];
        let checkout_path = checkout.path().to_string_lossy().into_owned();
        prepare_home_with(dir.path(), &me, &peers, |cfg| {
            cfg.harnesses.insert(
                "fake".into(),
                Harness {
                    cmd: vec![FAKE_HARNESS.into(), "{prompt}".into()],
                    answer_path: "raw".into(),
                    enabled: true,
                    disabled_reason: None,
                    env: Default::default(),
                },
            );
            cfg.responder.harness = "fake".into();
            cfg.projects.insert(PROJECT.into(), checkout_path);
            tweak(cfg);
        });
        Home {
            dir,
            _checkout: checkout,
            me,
            maciek,
            ana,
            maciej,
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn spool(&self) -> Spool {
        Spool::new(self.path()).unwrap()
    }

    fn owl(&self) -> Command {
        let mut c = Command::new(OWL);
        c.env_remove("OWLPOST_HOME")
            .env_remove("EDITOR")
            .arg("--home")
            .arg(self.path());
        c
    }

    /// Runs `owl <args>` and returns (exit code, stdout, stderr).
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = self.owl().args(args).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8(out.stdout).unwrap(),
            String::from_utf8(out.stderr).unwrap(),
        )
    }

    fn ok(&self, args: &[&str]) -> String {
        let (code, out, err) = self.run(args);
        assert_eq!(code, 0, "owl {args:?} failed: {err}");
        out
    }

    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_str(&self.ok(args)).expect("valid JSON output")
    }

    /// Exit 1 with `needle` on stderr.
    fn fails(&self, args: &[&str], needle: &str) -> String {
        let (code, out, err) = self.run(args);
        assert_eq!(code, 1, "owl {args:?}: stdout {out:?} stderr {err:?}");
        assert!(
            err.contains(needle),
            "owl {args:?}: stderr {err:?} lacks {needle:?}"
        );
        err
    }

    /// Spools a question from `from` to me in `state` the way the daemon does (§3.2); returns the id.
    fn put(&self, from: &Identity, text: &str, state: &str) -> String {
        let env = signed(from, &self.me, text);
        self.put_env(&env, state)
    }

    fn put_env(&self, env: &Envelope, state: &str) -> String {
        let p: Payload = serde_json::from_str(&env.raw).unwrap();
        let hash = match &p.body {
            Body::Question {
                project,
                path,
                question,
            } => envelope::question_hash(project, path, question),
            Body::Answer { .. } => String::new(),
        };
        let rec = Record {
            raw: env.raw.clone(),
            sig: env.sig.clone(),
            state: state.into(),
            seen: false,
            received_at: envelope::rfc3339_now(),
            draft: None,
            meta: json!({ "peer": p.from, "hash": hash }),
        };
        self.spool().put(Dir::Inbox, &p.id, &rec).unwrap();
        p.id
    }

    fn inbox(&self, id: &str) -> Option<Record> {
        self.spool().get(Dir::Inbox, id).unwrap()
    }

    fn done(&self, id: &str) -> Option<Record> {
        self.spool().get(Dir::Done, id).unwrap()
    }

    fn outbox(&self) -> Vec<(String, Record)> {
        self.spool().list(Dir::Outbox, |_| true).unwrap()
    }

    fn set_received(&self, id: &str, dir: Dir, at: &str) {
        let spool = self.spool();
        let mut r = spool.get(dir, id).unwrap().unwrap();
        r.received_at = at.into();
        spool.put(dir, id, &r).unwrap();
    }

    fn set_meta(&self, id: &str, meta: Value) {
        let spool = self.spool();
        let mut r = spool.get(Dir::Inbox, id).unwrap().unwrap();
        r.meta = meta;
        spool.put(Dir::Inbox, id, &r).unwrap();
    }

    /// Spools an envelope straight into `done/` in `state`, the way the daemon's ack path
    /// (outbox → done, `acked`) or a finished exchange leaves it; returns the id.
    fn put_done(&self, env: &Envelope, state: &str) -> String {
        let p: Payload = serde_json::from_str(&env.raw).unwrap();
        let rec = Record {
            raw: env.raw.clone(),
            sig: env.sig.clone(),
            state: state.into(),
            seen: true,
            received_at: envelope::rfc3339_now(),
            draft: None,
            meta: json!({}),
        };
        self.spool().put(Dir::Done, &p.id, &rec).unwrap();
        p.id
    }

    fn editor_script(&self, body: &str) -> String {
        let p = self.path().join("editor.sh");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p.to_string_lossy().into_owned()
    }
}

fn payload(rec: &Record) -> Payload {
    serde_json::from_str(&rec.raw).unwrap()
}

fn answer_body(p: &Payload) -> (String, String, u32, bool) {
    match &p.body {
        Body::Answer {
            answer,
            harness,
            redactions,
            cached,
        } => (answer.clone(), harness.clone(), *redactions, *cached),
        Body::Question { .. } => panic!("expected an answer payload"),
    }
}

// ---------------------------------------------------------------- AC1

#[test]
fn count_and_formats() {
    let h = Home::new();
    h.put(&h.maciek, "Why is the refresh token rotated?", "pending");
    h.put(&h.maciek, "Where is the retry policy?", "consent");

    assert_eq!(h.ok(&["inbox", "--count"]), "2\n");
    let claude = h.ok(&["inbox", "--count", "--format", "claude"]);
    assert_eq!(claude.lines().count(), 1, "one line: {claude:?}");
    assert_eq!(claude, format!("{CLAUDE_TWO}\n"));
    let v: Value = serde_json::from_str(claude.trim()).unwrap();
    let hook = &v["hookSpecificOutput"];
    assert_eq!(hook["hookEventName"], "UserPromptSubmit");
    assert!(
        hook["additionalContext"]
            .as_str()
            .unwrap()
            .starts_with("owlpost: 2 new questions"),
        "{hook}"
    );
    assert_eq!(h.ok(&["inbox", "--count", "--format", "codex"]), claude);
    assert_eq!(
        h.ok(&["inbox", "--count", "--format", "kimi"]),
        format!("{SENTENCE_TWO}\n")
    );
    assert_eq!(
        h.ok(&["inbox", "--count", "--format", "plain"]),
        format!("{SENTENCE_TWO}\n")
    );
    // Counting never marks anything seen.
    assert_eq!(h.ok(&["inbox", "--count"]), "2\n");
    let (code, out, err) = h.run(&["inbox", "--count", "--format", "vim"]);
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(err.contains("unknown --format vim"), "{err}");

    // Zero unseen: nothing at all on stdout, exit 0, for every format.
    h.ok(&["inbox"]);
    assert_eq!(h.ok(&["inbox", "--count"]), "0\n");
    for f in FORMATS {
        let (code, out, err) = h.run(&["inbox", "--count", "--format", f]);
        assert_eq!(
            (code, out.as_str(), err.as_str()),
            (0, "", ""),
            "format {f}"
        );
    }
    // `--count --all` keeps the total visible after everything was seen (§3.3).
    assert_eq!(h.ok(&["inbox", "--count", "--all"]), "2\n");
    assert_eq!(
        h.ok(&["inbox", "--count", "--all", "--format", "plain"]),
        format!("{SENTENCE_TWO}\n")
    );
    let v = h.json(&["inbox", "--count", "--json"]);
    assert_eq!(v["count"], 0);
    let v = h.json(&["inbox", "--count", "--all", "--json"]);
    assert_eq!(v["count"], 2);
    assert_eq!(v["questions"], 2);
    assert_eq!(v["peers"], json!([{ "name": "Maciek", "count": 2 }]));

    // An empty home (no spool yet) counts 0 and injects nothing.
    let empty = Home::new();
    assert_eq!(empty.ok(&["inbox", "--count"]), "0\n");
    for f in FORMATS {
        assert_eq!(empty.ok(&["inbox", "--count", "--format", f]), "", "{f}");
    }
}

#[test]
fn hook_line_singular_and_per_peer_counts() {
    let h = Home::new();
    h.put(&h.maciek, "one?", "pending");
    for f in FORMATS {
        let out = h.ok(&["inbox", "--count", "--format", f]);
        let expected =
            "owlpost: 1 new question (Maciek 1). Say \"show owlpost inbox\" or run `owl inbox`.";
        match f {
            "claude" | "codex" => assert_eq!(
                out,
                format!(
                    "{{\"hookSpecificOutput\":{{\"hookEventName\":\"UserPromptSubmit\",\"additionalContext\":{}}}}}\n",
                    serde_json::to_string(expected).unwrap()
                ),
                "{f}"
            ),
            _ => assert_eq!(out, format!("{expected}\n"), "{f}"),
        }
    }
    // Two peers: ordered by count descending, then name; an unknown peer shows its fingerprint.
    h.put(&h.ana, "two?", "pending");
    h.put(&h.ana, "three?", "pending");
    assert_eq!(
        h.ok(&["inbox", "--count", "--format", "plain"]),
        "owlpost: 3 new questions (Ana 2, Maciek 1). Say \"show owlpost inbox\" or run `owl inbox`.\n"
    );
    let stranger = id(9);
    h.put(&stranger, "four?", "consent");
    assert_eq!(
        h.ok(&["inbox", "--count", "--format", "kimi"]),
        format!(
            "owlpost: 4 new questions (Ana 2, Maciek 1, {} 1). Say \"show owlpost inbox\" or run `owl inbox`.\n",
            fp(&stranger)
        )
    );
}

// ---------------------------------------------------------------- AC2

#[test]
fn listing_marks_seen() {
    let h = Home::new();
    let a = h.put(&h.maciek, "first?", "pending");
    let b = h.put(&h.ana, "second?", "consent");
    h.set_received(
        &a,
        Dir::Inbox,
        &envelope::unix_to_rfc3339(envelope::now_unix() - 7_200),
    );

    let out = h.ok(&["inbox"]);
    assert!(out.contains(&a) && out.contains(&b), "{out}");
    assert!(out.contains("Maciek") && out.contains("Ana"), "{out}");
    assert!(out.contains("pending") && out.contains("consent"), "{out}");
    assert!(out.contains(PATH), "{out}");
    assert!(
        out.lines().any(|l| l.contains(&a) && l.ends_with("2h")),
        "{out}"
    );
    assert_eq!(h.ok(&["inbox", "--count"]), "0\n");
    assert!(h.inbox(&a).unwrap().seen && h.inbox(&b).unwrap().seen);

    let (code, out, err) = h.run(&["inbox", "--new"]);
    assert_eq!((code, out.as_str(), err.as_str()), (0, "", ""));
    let all = h.ok(&["inbox", "--all"]);
    assert!(all.contains(&a) && all.contains(&b), "{all}");
    // The default listing keeps showing seen records too (§3.3 "lists everything").
    let again = h.ok(&["inbox"]);
    assert!(again.contains(&a) && again.contains(&b), "{again}");

    // A third, unseen record: `--new` lists exactly that one and marks it.
    let c = h.put(&h.maciek, "third?", "pending");
    let new = h.ok(&["inbox", "--new"]);
    assert!(
        new.contains(&c) && !new.contains(&a) && !new.contains(&b),
        "{new}"
    );
    assert!(h.inbox(&c).unwrap().seen);
    assert_eq!(h.ok(&["inbox", "--new"]), "");

    let rows = h.json(&["inbox", "--json"]);
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 3);
    let row = rows.iter().find(|r| r["id"] == a).unwrap();
    assert_eq!(row["from_name"], "Maciek");
    assert_eq!(row["from"], fp(&h.maciek));
    assert_eq!(row["type"], "question");
    assert_eq!(row["state"], "pending");
    assert_eq!(row["path"], PATH);
    assert_eq!(row["project"], PROJECT);
    assert_eq!(row["seen"], true);
    assert_eq!(row["age"], "2h");
    assert!(row["age_secs"].as_u64().unwrap() >= 7_200);
    assert_eq!(h.json(&["inbox", "--new", "--json"]), json!([]));
}

#[test]
fn show_prints_content_and_marks_seen() {
    let h = Home::new();
    let a = h.put(
        &h.maciek,
        "Why is the refresh token rotated on every read?",
        "pending",
    );
    let b = h.put(&h.ana, "second?", "consent");
    assert_eq!(h.ok(&["inbox", "--count"]), "2\n");

    let out = h.ok(&["show", &a]);
    assert!(
        out.contains("Why is the refresh token rotated on every read?"),
        "{out}"
    );
    assert!(
        out.contains("Maciek") && out.contains(&fp(&h.maciek)),
        "{out}"
    );
    assert!(out.contains(PROJECT) && out.contains(PATH), "{out}");
    assert!(out.contains("state:    pending"), "{out}");
    assert!(!out.contains("draft"), "no draft yet: {out}");
    assert_eq!(h.ok(&["inbox", "--count"]), "1\n");
    assert!(h.inbox(&a).unwrap().seen && !h.inbox(&b).unwrap().seen);

    let all = h.ok(&["show", "all"]);
    assert!(
        all.contains(&a) && all.contains(&b) && all.contains("second?"),
        "{all}"
    );
    assert_eq!(h.ok(&["inbox", "--count"]), "0\n");

    h.ok(&["draft", &a]);
    let out = h.ok(&["show", &a]);
    assert!(out.contains("draft (ok via fake, redactions: 1"), "{out}");
    assert!(out.contains("[redacted]"), "{out}");
    let v = h.json(&["show", &a, "--json"]);
    assert_eq!(v["id"], a);
    assert_eq!(v["state"], "drafted");
    assert_eq!(
        v["payload"]["body"]["question"],
        "Why is the refresh token rotated on every read?"
    );
    assert_eq!(v["draft"]["redactions"], 1);
    assert!(v["draft"]["text"].as_str().unwrap().contains("[redacted]"));
    assert!(h.json(&["show", "all", "--json"]).is_array());

    // An answer record shows the answer text.
    let q = question(&h.me, &h.ana, "asked earlier?");
    let ans = Envelope::sign(
        &Payload::answer(&q, "Because of X.", "claude", 0, false),
        &h.ana,
    );
    let c = h.put_env(&ans, "pending");
    let out = h.ok(&["show", &c]);
    assert!(
        out.contains("type:     answer") && out.contains("Because of X."),
        "{out}"
    );
    assert!(out.contains(&format!("reply to: {}", q.id)), "{out}");

    h.fails(&["show", "nope"], "no inbox record nope");
    let (code, out, err) = h.run(&["show", "nope", "--json"]);
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(err.contains("nope"), "{err}");
}

// ---------------------------------------------------------------- AC3

#[test]
fn draft_send_moves_records() {
    let h = Home::new();
    let qid = h.put(&h.maciek, "Where is the retry policy defined?", "pending");
    let q = payload(&h.inbox(&qid).unwrap());

    let out = h.ok(&["draft", &qid]);
    assert!(out.contains("[redacted]"), "{out}");
    assert!(!out.contains("sk-test-123456"), "secret leaked: {out}");
    assert!(out.contains("redactions: 1"), "{out}");
    assert!(out.contains("harness: fake"), "{out}");
    let rec = h.inbox(&qid).unwrap();
    assert_eq!(rec.state, "drafted");
    let d = rec.draft.as_ref().unwrap();
    assert!(d["text"].as_str().unwrap().contains("[redacted]"));
    assert_eq!(d["harness"], "fake");
    assert_eq!(d["redactions"], 1);
    assert_eq!(d["status"], "ok");
    assert!(envelope::parse_rfc3339_to_unix(d["drafted_at"].as_str().unwrap()).is_some());
    assert!(h.outbox().is_empty(), "draft must not touch outbox");

    let out = h.ok(&["send", &qid]);
    let outbox = h.outbox();
    assert_eq!(outbox.len(), 1, "{outbox:?}");
    let (aid, arec) = &outbox[0];
    assert!(out.contains(&format!("sent {aid}")), "{out}");
    assert_eq!(arec.state, "unacked");
    assert!(!arec.seen && arec.draft.is_none());
    assert_eq!(arec.meta["question_id"], qid);
    assert_eq!(arec.meta["peer"], fp(&h.maciek));
    let env = Envelope {
        raw: arec.raw.clone(),
        sig: arec.sig.clone(),
    };
    let a = env
        .verify(&h.me.verifying_key())
        .expect("signature verifies against the responder key");
    assert!(
        env.verify(&h.maciek.verifying_key()).is_err(),
        "not signed by the asker"
    );
    assert_eq!(a.id, *aid);
    assert_eq!(a.kind, Kind::Answer);
    assert_eq!(a.in_reply_to.as_deref(), Some(qid.as_str()));
    assert_eq!(a.from, fp(&h.me));
    assert_eq!(a.to, fp(&h.maciek));
    let (text, harness, redactions, cached) = answer_body(&a);
    assert!(
        text.contains("[redacted]") && text.contains("src/client.rs"),
        "{text}"
    );
    assert_eq!((harness.as_str(), redactions, cached), ("fake", 1, false));
    // Raw JSON keeps the §6 answer body shape.
    let raw: Value = serde_json::from_str(&arec.raw).unwrap();
    assert_eq!(raw["type"], "answer");
    assert_eq!(raw["body"]["redactions"], 1);
    assert_eq!(raw["body"]["cached"], false);
    assert_eq!(raw["body"]["harness"], "fake");

    assert!(h.inbox(&qid).is_none(), "question left the inbox");
    let done = h.done(&qid).unwrap();
    assert_eq!(done.state, "answered");
    assert_eq!(done.raw, rec.raw, "the question record itself moved");
    assert_eq!(done.meta["answer_id"], *aid);
    assert!(envelope::parse_rfc3339_to_unix(done.meta["done_at"].as_str().unwrap()).is_some());

    let hash = match &q.body {
        Body::Question {
            project,
            path,
            question,
        } => envelope::question_hash(project, path, question),
        Body::Answer { .. } => unreachable!(),
    };
    let cached = h
        .spool()
        .cache_get(&hash)
        .unwrap()
        .expect("cache holds the answer");
    assert_eq!(
        (cached.raw.as_str(), cached.sig.as_str()),
        (arec.raw.as_str(), arec.sig.as_str())
    );
    // The same question with different whitespace/case normalises to the same hash.
    let same = envelope::question_hash(PROJECT, PATH, "  where IS the retry   policy defined? ");
    assert!(h.spool().cache_get(&same).unwrap().is_some());
    assert_eq!(h.ok(&["inbox", "--count", "--all"]), "0\n");
    let hist = h.json(&["history", "--json"]);
    assert_eq!(hist[0]["id"], qid);
    assert_eq!(hist[0]["state"], "answered");
    assert_eq!(hist[0]["peer_name"], "Maciek");

    // Re-sending is refused: the record is gone from the inbox and the outbox stays single.
    h.fails(&["send", &qid], &format!("no inbox record {qid}"));
    assert_eq!(h.outbox().len(), 1);
    let v = h.json(&["draft", "--json", &h.put(&h.ana, "json draft?", "pending")]);
    assert_eq!(v["state"], "drafted");
    assert_eq!(v["redactions"], 1);
}

/// A stored record whose `meta` is not an object (a scalar, an array, null) must not panic
/// `send`/`reject` half-way through the move: the command exits 0 and `done/` holds the record
/// with its final state and the new meta keys. An object `meta` keeps its existing keys.
#[test]
fn reject_and_send_survive_non_object_meta() {
    let h = Home::new();
    let shapes = [json!("oops"), json!([1]), Value::Null, json!(7)];
    for bad in &shapes {
        let r = h.put(&h.maciek, &format!("reject with meta {bad}?"), "pending");
        h.set_meta(&r, bad.clone());
        let (code, out, err) = h.run(&["reject", &r]);
        assert_eq!(
            (code, out.as_str()),
            (0, format!("rejected {r}\n").as_str()),
            "{bad}: {err}"
        );
        assert!(h.inbox(&r).is_none(), "{bad}: left the inbox");
        let done = h.done(&r).unwrap();
        assert_eq!(done.state, "rejected", "{bad}");
        assert_eq!(done.meta["previous_state"], "pending", "{bad}");
        assert!(done.meta["done_at"].is_string(), "{bad}: {}", done.meta);

        let s = h.put(&h.maciek, &format!("send with meta {bad}?"), "pending");
        h.ok(&["draft", &s]);
        h.set_meta(&s, bad.clone());
        let before = h.outbox().len();
        let (code, _, err) = h.run(&["send", &s]);
        assert_eq!(code, 0, "{bad}: {err}");
        let outbox = h.outbox();
        assert_eq!(outbox.len(), before + 1, "{bad}");
        let (aid, _) = outbox
            .iter()
            .find(|(_, a)| a.meta["question_id"] == s)
            .unwrap();
        assert!(h.inbox(&s).is_none(), "{bad}: left the inbox");
        let done = h.done(&s).unwrap();
        assert_eq!(done.state, "answered", "{bad}");
        assert_eq!(done.meta["answer_id"], *aid, "{bad}");
        assert!(done.meta["done_at"].is_string(), "{bad}: {}", done.meta);
    }
    // The positive twin: an object meta keeps what the daemon stored on it.
    let r = h.put(&h.ana, "object meta?", "pending");
    let peer = h.inbox(&r).unwrap().meta;
    assert_eq!(peer["peer"], fp(&h.ana));
    h.ok(&["reject", &r]);
    let done = h.done(&r).unwrap();
    assert_eq!(done.meta["peer"], peer["peer"]);
    assert_eq!(done.meta["hash"], peer["hash"]);
    assert_eq!(done.meta["previous_state"], "pending");
    // Nothing is left behind in the inbox in any state.
    assert!(h.spool().list(Dir::Inbox, |_| true).unwrap().is_empty());
}

#[tokio::test]
async fn sent_answer_is_served_from_the_daemon_cache() {
    let h = Home::new();
    let qid = h.put(&h.maciek, "Where is the retry policy defined?", "pending");
    h.ok(&["draft", &qid]);
    h.ok(&["send", &qid]);
    let sent = h.outbox().remove(0).1;

    let Home {
        dir, me, maciek, ..
    } = h;
    let daemon = respawn(dir, me).await;
    let cl = client(Some(&maciek), &daemon.id);
    // A fresh question with the same text and path: the daemon answers 200 from the cache
    // with exactly the envelope `owl send` wrote.
    let again = signed(&maciek, &daemon.id, "where is the RETRY policy defined?");
    let resp = post_envelope(&cl, &daemon, &again).await;
    assert_eq!(resp.status(), 200);
    let sig = resp
        .headers()
        .get("X-Owl-Signature")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let body = resp.text().await.unwrap();
    assert_eq!(
        (body.as_str(), sig.as_str()),
        (sent.raw.as_str(), sent.sig.as_str())
    );
    // And the outbox listing for Maciek carries the same envelope.
    let list: Vec<Value> = cl
        .get(daemon.url("/v1/outbox"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list, vec![json!({ "raw": sent.raw, "sig": sent.sig })]);
    daemon.running.shutdown();
}

// ---------------------------------------------------------------- AC4

#[test]
fn edit_replaces_draft() {
    let h = Home::new();
    let qid = h.put(&h.maciek, "edit me?", "pending");
    h.fails(&["edit", &qid], &format!("owl draft {qid}"));
    h.ok(&["draft", &qid]);
    let before = h.inbox(&qid).unwrap().draft.unwrap()["text"]
        .as_str()
        .unwrap()
        .to_string();

    // No EDITOR: refused, draft unchanged.
    h.fails(&["edit", &qid], "EDITOR is not set");
    assert_eq!(h.inbox(&qid).unwrap().draft.unwrap()["text"], before);

    // A failing editor: refused, draft unchanged.
    let bad = h.editor_script("exit 3");
    let out = h
        .owl()
        .env("EDITOR", &bad)
        .args(["edit", &qid])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("exited with"));
    assert_eq!(h.inbox(&qid).unwrap().draft.unwrap()["text"], before);

    // The real edit appends EDITED; the editor receives the current draft text.
    let log = h.path().join("editor-saw.txt");
    let script = h.editor_script(&format!(
        "cp \"$1\" {}\necho EDITED >> \"$1\"",
        log.display()
    ));
    let out = h
        .owl()
        .env("EDITOR", &script)
        .args(["edit", &qid])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read_to_string(&log).unwrap(), before);
    let d = h.inbox(&qid).unwrap().draft.unwrap();
    let text = d["text"].as_str().unwrap();
    assert!(text.ends_with("EDITED"), "{text:?}");
    assert!(text.starts_with(&before), "{text:?}");
    assert_eq!(d["status"], "edited");
    assert_eq!(d["redactions"], 1);
    assert!(d["edited_at"].is_string());
    assert!(
        h.path().join("tmp").read_dir().unwrap().next().is_none(),
        "temp file cleaned up"
    );
    assert_eq!(h.inbox(&qid).unwrap().state, "drafted");

    h.ok(&["send", &qid]);
    let (_, arec) = h.outbox().remove(0);
    let a = Envelope {
        raw: arec.raw,
        sig: arec.sig,
    }
    .verify(&h.me.verifying_key())
    .unwrap();
    let (sent, _, redactions, _) = answer_body(&a);
    assert!(sent.ends_with("EDITED"), "{sent:?}");
    assert!(sent.contains("[redacted]"));
    assert_eq!(redactions, 1);

    // An editor that empties the file: refused, draft unchanged.
    let qid = h.put(&h.ana, "empty edit?", "pending");
    h.ok(&["draft", &qid]);
    let wipe = h.editor_script(": > \"$1\"");
    let out = h
        .owl()
        .env("EDITOR", &wipe)
        .args(["edit", &qid])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("is empty"));
    assert!(
        h.inbox(&qid).unwrap().draft.unwrap()["text"]
            .as_str()
            .unwrap()
            .contains("[redacted]")
    );
}

// ---------------------------------------------------------------- AC5

#[test]
fn reject_and_history() {
    let h = Home::new();
    let rejected = h.put(&h.maciek, "reject me?", "pending");
    let kept = h.put(&h.ana, "keep me?", "pending");
    let twin = h.put(&h.maciej, "twin?", "consent");

    assert_eq!(
        h.ok(&["reject", &rejected]),
        format!("rejected {rejected}\n")
    );
    assert!(h.inbox(&rejected).is_none());
    let done = h.done(&rejected).unwrap();
    assert_eq!(done.state, "rejected");
    assert_eq!(done.meta["previous_state"], "pending");
    assert!(h.inbox(&kept).is_some(), "other records untouched");
    assert!(h.outbox().is_empty());
    assert_eq!(
        h.json(&["reject", &twin, "--json"]),
        json!({ "id": twin, "state": "rejected" })
    );

    let hist = h.json(&["history", "--json"]);
    let ids = |v: &Value| -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect()
    };
    let mut expected = vec![rejected.clone(), twin.clone()];
    expected.sort();
    assert_eq!(
        ids(&hist),
        expected,
        "history lists exactly the two rejected records, sorted"
    );
    let mine = hist
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == rejected)
        .unwrap();
    assert_eq!(mine["state"], "rejected");
    assert_eq!(mine["peer_name"], "Maciek");
    assert_eq!(mine["peer"], fp(&h.maciek));
    assert_eq!(mine["path"], PATH);
    assert_eq!(mine["type"], "question");
    assert_eq!(mine["text"], "reject me?");
    assert!(mine["done_at"].is_string());
    assert!(!ids(&hist).contains(&kept), "still in the inbox");

    // --peer: exact name, case-insensitive name, fingerprint; the one-letter twin is excluded.
    assert_eq!(
        ids(&h.json(&["history", "--json", "--peer", "Maciek"])),
        vec![rejected.clone()]
    );
    assert_eq!(
        ids(&h.json(&["history", "--json", "--peer", "maciek"])),
        vec![rejected.clone()]
    );
    assert_eq!(
        ids(&h.json(&["history", "--json", "--peer", &fp(&h.maciek)])),
        vec![rejected.clone()]
    );
    assert_eq!(
        ids(&h.json(&["history", "--json", "--peer", "Maciej"])),
        vec![twin.clone()]
    );
    assert_eq!(h.json(&["history", "--json", "--peer", "Ana"]), json!([]));
    assert_eq!(
        h.json(&["history", "--json", "--peer", "Mac"]),
        json!([]),
        "no prefix matching"
    );

    // --since: relative and absolute, boundary inclusive.
    assert_eq!(
        ids(&h.json(&["history", "--json", "--since", "1d"])).len(),
        2
    );
    assert_eq!(
        ids(&h.json(&["history", "--json", "--since", "1h"])).len(),
        2
    );
    h.set_received(&rejected, Dir::Done, "2026-09-01T10:00:00Z");
    assert_eq!(
        h.json(&["history", "--json", "--since", "1d", "--peer", "Maciek"]),
        json!([])
    );
    assert_eq!(
        ids(&h.json(&[
            "history",
            "--json",
            "--since",
            "2026-09-01T10:00:00Z",
            "--peer",
            "Maciek"
        ])),
        vec![rejected.clone()]
    );
    assert_eq!(
        h.json(&[
            "history",
            "--json",
            "--since",
            "2026-09-01T10:00:01Z",
            "--peer",
            "Maciek"
        ]),
        json!([])
    );
    let (code, _, err) = h.run(&["history", "--since", "yesterday"]);
    assert_eq!(code, 1);
    assert!(err.contains("bad --since"), "{err}");

    // --path glob: one character apart between the positive and the negative pattern.
    assert_eq!(
        ids(&h.json(&["history", "--json", "--path", "src/auth/*.rs"])).len(),
        2
    );
    assert_eq!(
        h.json(&["history", "--json", "--path", "src/auth/*.ts"]),
        json!([])
    );
    assert_eq!(
        ids(&h.json(&["history", "--json", "--path", "src/auth/session.r?"])).len(),
        2
    );
    assert_eq!(
        h.json(&["history", "--json", "--path", "src/auth/session.?"]),
        json!([])
    );
    assert_eq!(
        ids(&h.json(&["history", "--json", "--path", PATH])).len(),
        2
    );
    assert_eq!(
        h.json(&["history", "--json", "--path", "src/auth"]),
        json!([])
    );

    // Plain output and the filters combined.
    let plain = h.ok(&["history"]);
    assert!(
        plain.contains(&rejected) && plain.contains("Maciek") && plain.contains("rejected"),
        "{plain}"
    );
    assert_eq!(h.ok(&["history", "--peer", "Ana"]), "");
    assert_eq!(
        ids(&h.json(&[
            "history", "--json", "--peer", "Maciej", "--path", "*.rs", "--since", "1d"
        ])),
        vec![twin.clone()]
    );

    // Reject works from every inbox state, and refuses unknown ids.
    let c = h.put(&h.maciek, "consent reject?", "consent");
    let d = h.put(&h.maciek, "drafted reject?", "pending");
    h.ok(&["draft", &d]);
    h.ok(&["reject", &c]);
    h.ok(&["reject", &d]);
    assert_eq!(h.done(&c).unwrap().state, "rejected");
    assert_eq!(h.done(&d).unwrap().state, "rejected");
    assert!(
        h.done(&d).unwrap().draft.is_some(),
        "the draft stays on the rejected record"
    );
    h.fails(&["reject", "nope"], "no inbox record nope");
    h.fails(
        &["reject", &rejected],
        &format!("no inbox record {rejected}"),
    );
}

/// `peer` is the *other* party: for a record this identity sent (an acked answer moved to
/// `done/` by the daemon) it is the recipient, not `from`; for a received one it is the sender.
#[test]
fn history_names_the_other_party_for_records_this_identity_sent() {
    let h = Home::new();
    let q = question(&h.maciek, &h.me, "asked by maciek?");
    let mine = h.put_done(
        &Envelope::sign(
            &Payload::answer(&q, "Because of Y.", "fake", 0, false),
            &h.me,
        ),
        "acked",
    );
    let theirs = h.put(&h.ana, "asked by ana?", "pending");
    h.ok(&["reject", &theirs]);

    let rows = h.json(&["history", "--json"]);
    let row = |id: &str| -> Value {
        rows.as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("{id} missing from {rows}"))
    };
    let sent = row(&mine);
    assert_eq!(sent["from"], fp(&h.me));
    assert_eq!(sent["to"], fp(&h.maciek));
    assert_eq!(sent["peer"], fp(&h.maciek), "peer is the recipient");
    assert_eq!(sent["peer_name"], "Maciek");
    assert_eq!(sent["type"], "answer");
    assert_eq!(sent["state"], "acked");
    assert_eq!(sent["path"], "-");
    assert_eq!(sent["text"], "Because of Y.");
    let got = row(&theirs);
    assert_eq!(got["peer"], fp(&h.ana), "peer is the sender");
    assert_eq!(got["peer_name"], "Ana");

    // `--peer` follows the same rule: the sent answer is Maciek's exchange, never "mine".
    let ids = |v: Value| -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        ids(h.json(&["history", "--json", "--peer", "Maciek"])),
        vec![mine.clone()]
    );
    assert_eq!(
        ids(h.json(&["history", "--json", "--peer", &fp(&h.maciek)])),
        vec![mine.clone()]
    );
    assert_eq!(
        h.json(&["history", "--json", "--peer", &fp(&h.me)]),
        json!([])
    );
    assert_eq!(
        ids(h.json(&["history", "--json", "--peer", "Ana"])),
        vec![theirs]
    );
    let plain = h.ok(&["history", "--peer", "Maciek"]);
    assert!(
        plain.contains(&mine) && plain.contains("Maciek") && plain.contains("answer"),
        "{plain}"
    );
}

// ---------------------------------------------------------------- AC6

#[test]
fn draft_on_consent_points_to_allow() {
    let h = Home::new();
    let c = h.put(&h.maciek, "consent?", "consent");
    let err = h.fails(&["draft", &c], "owl allow");
    assert!(
        err.contains(&fp(&h.maciek)),
        "names the peer to allow: {err}"
    );
    let rec = h.inbox(&c).unwrap();
    assert_eq!(rec.state, "consent");
    assert!(rec.draft.is_none());
    assert!(
        h.path()
            .join("tmp")
            .read_dir()
            .map(|mut d| d.next().is_none())
            .unwrap_or(true),
        "runner never ran"
    );

    // Other states: denied is refused, an answer record is refused, unknown id is refused.
    let denied = h.put(&h.ana, "denied?", "denied");
    h.fails(&["draft", &denied], "state denied");
    assert!(h.inbox(&denied).unwrap().draft.is_none());
    let q = question(&h.me, &h.ana, "asked earlier?");
    let ans = h.put_env(
        &Envelope::sign(&Payload::answer(&q, "Because.", "claude", 0, false), &h.ana),
        "pending",
    );
    h.fails(&["draft", &ans], "is an answer");
    h.fails(&["draft", "nope"], "no inbox record nope");
    // Unknown harness and unknown project surface the runner's error, nothing stored.
    let p = h.put(&h.ana, "bad harness?", "pending");
    h.fails(&["draft", &p, "--harness", "nope"], "unknown harness nope");
    assert_eq!(h.inbox(&p).unwrap().state, "pending");

    // pending → drafted, and a re-draft on drafted is allowed (new drafted_at).
    let p = h.put(&h.maciek, "pending?", "pending");
    h.ok(&["draft", &p]);
    let first = h.inbox(&p).unwrap();
    assert_eq!(first.state, "drafted");
    let mut edited = first.clone();
    edited.draft.as_mut().unwrap()["text"] = json!("hand written");
    h.spool().put(Dir::Inbox, &p, &edited).unwrap();
    h.ok(&["draft", &p]);
    let second = h.inbox(&p).unwrap();
    assert_eq!(second.state, "drafted");
    assert!(
        second.draft.unwrap()["text"]
            .as_str()
            .unwrap()
            .contains("[redacted]"),
        "re-drafted"
    );
}

#[test]
fn draft_timeout_is_stored_but_exits_1() {
    let h = Home::with(|cfg| {
        cfg.responder.timeout_secs = 1;
        cfg.harnesses
            .get_mut("fake")
            .unwrap()
            .env
            .insert("FAKE_SLEEP".into(), "3".into());
    });
    let p = h.put(&h.maciek, "slow?", "pending");
    let (code, _, err) = h.run(&["draft", &p]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("status timeout"), "{err}");
    let rec = h.inbox(&p).unwrap();
    assert_eq!(rec.state, "drafted");
    assert_eq!(rec.draft.unwrap()["status"], "timeout");
    // The unreviewed timeout draft can still be sent deliberately: it is the human's call.
    h.ok(&["send", &p]);
    assert_eq!(h.done(&p).unwrap().state, "answered");
}

/// The runner's `extract_failed` outcome (§10, OWL-009): the harness printed something that
/// `answer_path` cannot read. The raw (redacted) stdout is stored as the draft, the command
/// prints it and exits 1 with a pointer to `owl edit`, exactly like `timeout`. The positive
/// twin differs only in the harness output: valid `result` JSON drafts with status `ok`, exit 0.
#[test]
fn draft_extract_failed_is_stored_but_exits_1() {
    let with_result_path = |output_file: Option<String>| {
        Home::with(|cfg| {
            let fake = cfg.harnesses.get_mut("fake").unwrap();
            fake.answer_path = "result".into();
            if let Some(f) = output_file {
                fake.env.insert("FAKE_OUTPUT_FILE".into(), f);
            }
        })
    };
    // Plain text through `answer_path = result`: not JSON, so extraction fails.
    let h = with_result_path(None);
    let p = h.put(&h.maciek, "unparsable?", "pending");
    let (code, out, err) = h.run(&["draft", &p]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("status extract_failed"), "{err}");
    assert!(err.contains(&format!("owl edit {p}")), "{err}");
    assert!(
        out.contains("[redacted]"),
        "raw stdout is still redacted: {out}"
    );
    assert!(!out.contains("sk-test-123456"), "secret leaked: {out}");
    let rec = h.inbox(&p).unwrap();
    assert_eq!(rec.state, "drafted");
    let d = rec.draft.unwrap();
    assert_eq!(d["status"], "extract_failed");
    assert!(
        d["text"].as_str().unwrap().contains("src/client.rs"),
        "raw stdout kept: {d}"
    );
    let v = h.json(&["show", &p, "--json"]);
    assert_eq!(v["draft"]["status"], "extract_failed");
    // `--json` reports the status too, still exit 1.
    let (code, out, _) = h.run(&["draft", "--json", &p]);
    assert_eq!(code, 1);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["status"], "extract_failed");
    assert_eq!(v["state"], "drafted");

    // Same harness, valid `{"type":"result","result":...}` output: status ok, exit 0.
    let good = with_result_path(None);
    let file = good.path().join("claude.json");
    std::fs::write(
        &file,
        r#"{"type":"result","is_error":false,"result":"The policy lives in src/client.rs."}"#,
    )
    .unwrap();
    let good = with_result_path(Some(file.to_string_lossy().into_owned()));
    let p = good.put(&good.maciek, "parsable?", "pending");
    let out = good.ok(&["draft", &p]);
    assert!(out.contains("The policy lives in src/client.rs."), "{out}");
    let d = good.inbox(&p).unwrap().draft.unwrap();
    assert_eq!(d["status"], "ok");
    assert_eq!(d["text"], "The policy lives in src/client.rs.");
}

// ---------------------------------------------------------------- AC7

#[test]
fn send_without_draft_points_to_draft() {
    let h = Home::new();
    let p = h.put(&h.maciek, "no draft?", "pending");
    h.fails(&["send", &p], &format!("owl draft {p}"));
    assert_eq!(h.inbox(&p).unwrap().state, "pending", "record untouched");
    assert!(h.done(&p).is_none());
    assert!(h.outbox().is_empty());
    assert!(h.spool().list(Dir::Cache, |_| true).unwrap().is_empty());

    // consent without a draft, and pending WITH a stale draft, are both refused.
    let c = h.put(&h.maciek, "consent?", "consent");
    h.fails(&["send", &c], "owl draft");
    let mut stale = h.inbox(&p).unwrap();
    stale.draft = Some(json!({
        "text": "t", "harness": "fake", "redactions": 0, "status": "ok",
        "drafted_at": "2026-09-01T10:00:00Z"
    }));
    h.spool().put(Dir::Inbox, &p, &stale).unwrap();
    h.fails(&["send", &p], "state pending, not drafted");
    assert!(h.outbox().is_empty());

    // A malformed draft object is a clean error, not a panic.
    let m = h.put(&h.ana, "malformed?", "pending");
    let mut rec = h.inbox(&m).unwrap();
    rec.state = "drafted".into();
    rec.draft = Some(json!({ "text": 42 }));
    h.spool().put(Dir::Inbox, &m, &rec).unwrap();
    h.fails(&["send", &m], "draft is malformed");
    h.fails(&["edit", &m], "draft is malformed");
    h.fails(&["show", &m], "draft is malformed");
    assert!(h.outbox().is_empty());

    // An answer record cannot be sent; a question addressed to someone else cannot either.
    let q = question(&h.me, &h.ana, "asked earlier?");
    let ans = h.put_env(
        &Envelope::sign(&Payload::answer(&q, "Because.", "claude", 0, false), &h.ana),
        "pending",
    );
    h.fails(&["send", &ans], "is an answer");
    let foreign = signed(&h.maciek, &h.ana, "for ana?");
    let f = h.put_env(&foreign, "drafted");
    let mut rec = h.inbox(&f).unwrap();
    rec.draft = stale.draft.clone();
    h.spool().put(Dir::Inbox, &f, &rec).unwrap();
    h.fails(&["send", &f], "not to this identity");
    assert!(h.outbox().is_empty());
    h.fails(&["send", "nope"], "no inbox record nope");
}

// ---------------------------------------------------------------- finish() failure paths

/// No `<id>.json.tmp` left behind in `dir` (OWL-003 rule for every temp-file write).
fn no_tmp_files(h: &Home, dir: Dir) {
    let d = h.path().join("spool").join(dir.name());
    let leftovers: Vec<String> = std::fs::read_dir(&d)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{}: {leftovers:?}", d.display());
}

/// `done/<id>.json` blocked by a directory: `owl send` exits 1, the inbox record is left
/// byte-for-byte as it was (state `drafted`), and the retry after unblocking succeeds while
/// reusing the one envelope already in `outbox/`.
#[test]
fn send_with_blocked_done_leaves_inbox_intact_and_retries_once() {
    let h = Home::new();
    let qid = h.put(&h.maciek, "Where is the retry policy defined?", "pending");
    h.ok(&["draft", &qid]);
    let before = h.inbox(&qid).unwrap();
    assert_eq!(before.state, "drafted");
    let block = h.spool().path(Dir::Done, &qid);
    std::fs::create_dir(&block).unwrap();

    let err = h.fails(&["send", &qid], &format!("finishing record {qid}"));
    assert!(err.starts_with("owl: "), "{err}");
    assert_eq!(err.lines().count(), 1, "one clean line: {err}");
    assert_eq!(
        h.inbox(&qid).unwrap(),
        before,
        "inbox record untouched by the failed send"
    );
    assert!(block.is_dir(), "the blocking directory is still there");
    let outbox = h.outbox();
    assert_eq!(
        outbox.len(),
        1,
        "the answer was already spooled: {outbox:?}"
    );
    let (aid, arec) = &outbox[0];
    assert_eq!(arec.meta["question_id"], qid);
    no_tmp_files(&h, Dir::Done);
    no_tmp_files(&h, Dir::Inbox);

    // Still wedged: a second attempt is the same clean failure, still one envelope.
    h.fails(&["send", &qid], &format!("finishing record {qid}"));
    assert_eq!(h.inbox(&qid).unwrap(), before);
    assert_eq!(h.outbox().len(), 1);

    std::fs::remove_dir(&block).unwrap();
    let out = h.ok(&["send", &qid]);
    assert_eq!(
        out,
        format!("sent {aid} (reply to {qid}, to {})\n", fp(&h.maciek)),
        "the retry reports the envelope written by the first attempt"
    );
    let outbox = h.outbox();
    assert_eq!(
        outbox.len(),
        1,
        "exactly one envelope after the retry: {outbox:?}"
    );
    assert_eq!(&outbox[0].0, aid);
    assert_eq!(outbox[0].1, *arec, "the envelope was not re-signed");
    assert!(h.inbox(&qid).is_none());
    let done = h.done(&qid).unwrap();
    assert_eq!(done.state, "answered");
    assert_eq!(done.meta["answer_id"], *aid);
    assert!(done.meta["done_at"].is_string());
    assert_eq!(done.raw, before.raw);
    assert_eq!(done.draft, before.draft);
    let hash = before.meta["hash"].as_str().unwrap();
    assert_eq!(h.spool().cache_get(hash).unwrap().unwrap().raw, arec.raw);
    no_tmp_files(&h, Dir::Done);
    no_tmp_files(&h, Dir::Inbox);

    // A finished question cannot be sent twice.
    h.fails(&["send", &qid], &format!("no inbox record {qid}"));
    assert_eq!(h.outbox().len(), 1);
}

/// Same block for `owl reject`: exit 1, record untouched in its original state, and the retry
/// records the original state (not `rejected`) as `previous_state`.
#[test]
fn reject_with_blocked_done_leaves_inbox_intact_and_retries() {
    let h = Home::new();
    let qid = h.put(&h.maciek, "reject me?", "consent");
    let before = h.inbox(&qid).unwrap();
    let block = h.spool().path(Dir::Done, &qid);
    std::fs::create_dir(&block).unwrap();

    let err = h.fails(&["reject", &qid], &format!("finishing record {qid}"));
    assert_eq!(err.lines().count(), 1, "one clean line: {err}");
    let after = h.inbox(&qid).unwrap();
    assert_eq!(after, before, "inbox record untouched by the failed reject");
    assert_eq!(after.state, "consent");
    assert!(after.meta.get("previous_state").is_none());
    assert!(after.meta.get("done_at").is_none());
    assert!(h.outbox().is_empty());
    no_tmp_files(&h, Dir::Done);
    no_tmp_files(&h, Dir::Inbox);

    std::fs::remove_dir(&block).unwrap();
    assert_eq!(h.ok(&["reject", &qid]), format!("rejected {qid}\n"));
    assert!(h.inbox(&qid).is_none());
    let done = h.done(&qid).unwrap();
    assert_eq!(done.state, "rejected");
    assert_eq!(
        done.meta["previous_state"], "consent",
        "the retry keeps the original state, not `rejected`"
    );
    assert!(done.meta["done_at"].is_string());
    no_tmp_files(&h, Dir::Done);
}

/// `send` reuses an outbox envelope for this question even when `finish` never failed — e.g.
/// the inbox file was restored from a backup — but never one answering a different question.
#[test]
fn send_reuses_only_the_envelope_for_this_question() {
    let h = Home::new();
    let first = h.put(&h.maciek, "first?", "pending");
    let second = h.put(&h.maciek, "second?", "pending");
    h.ok(&["draft", &first]);
    h.ok(&["draft", &second]);
    h.ok(&["send", &first]);
    assert_eq!(h.outbox().len(), 1);

    // Restore the finished record into the inbox as if from a backup: the answer is reused.
    let mut restored = h.done(&first).unwrap();
    restored.state = "drafted".into();
    h.spool().put(Dir::Inbox, &first, &restored).unwrap();
    std::fs::remove_file(h.spool().path(Dir::Done, &first)).unwrap();
    h.ok(&["send", &first]);
    assert_eq!(
        h.outbox().len(),
        1,
        "no second envelope for the same question"
    );

    // A different question gets its own envelope.
    h.ok(&["send", &second]);
    let outbox = h.outbox();
    assert_eq!(outbox.len(), 2, "{outbox:?}");
    let by_q: Vec<&str> = outbox
        .iter()
        .map(|(_, r)| r.meta["question_id"].as_str().unwrap())
        .collect();
    assert!(by_q.contains(&first.as_str()) && by_q.contains(&second.as_str()));
}

// ---------------------------------------------------------------- history without a key

/// With no identity key in the home, `owl history` still lists `done/` and names `from` as the
/// peer of every record — including one this identity sent, which with the key present would
/// have named `to` (the negative twin of `history_names_the_other_party_...`).
#[test]
fn history_without_identity_key_lists_done_with_from_as_peer() {
    let h = Home::new();
    let received = h.put(&h.maciek, "received?", "pending");
    h.ok(&["reject", &received]);
    let sent = h.put_done(&signed(&h.me, &h.maciek, "i asked?"), "acked");

    let with_key = h.json(&["history", "--json"]);
    let peer_of = |v: &Value, id: &str| -> (String, String) {
        let r = v
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("{id} missing from {v}"));
        (
            r["peer"].as_str().unwrap().to_string(),
            r["peer_name"].as_str().unwrap().to_string(),
        )
    };
    assert_eq!(
        peer_of(&with_key, &sent),
        (fp(&h.maciek), "Maciek".into()),
        "with the key, the other party of a sent record is `to`"
    );

    std::fs::remove_file(h.path().join("key")).unwrap();
    assert!(!h.path().join("key").exists());
    let (code, out, err) = h.run(&["history", "--json"]);
    assert_eq!(code, 0, "history must not need the key: {err}");
    assert_eq!(err, "");
    let no_key: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(no_key.as_array().unwrap().len(), 2, "{no_key}");
    assert_eq!(
        peer_of(&no_key, &received),
        (fp(&h.maciek), "Maciek".into())
    );
    assert_eq!(
        peer_of(&no_key, &sent),
        (fp(&h.me), fp(&h.me)),
        "without the key, peer falls back to `from` even for a record this identity sent"
    );

    // The plain table works too and shows the fallback peer column.
    let (code, out, _) = h.run(&["history"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("ID  "), "{out}");
    assert!(out.contains(&fp(&h.me)), "{out}");
    assert_eq!(out.lines().count(), 3, "{out}");
}

// ---------------------------------------------------------------- machine output shapes

#[test]
fn send_and_edit_json_shapes() {
    let h = Home::new();
    let qid = h.put(&h.maciek, "Where is sk-test-123456 used?", "pending");
    h.ok(&["draft", &qid]);
    let before = h.inbox(&qid).unwrap().draft.unwrap();

    let script = h.editor_script("echo EDITED >> \"$1\"");
    let out = h
        .owl()
        .env("EDITOR", &script)
        .args(["edit", &qid, "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let edited: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(edited["id"], qid);
    assert_eq!(edited["state"], "drafted");
    let d = &edited["draft"];
    assert_eq!(
        d["text"],
        format!("{}EDITED", before["text"].as_str().unwrap()),
        "the stored text has no trailing newline, so the editor appended straight onto it"
    );
    assert_eq!(d["status"], "edited");
    assert_eq!(d["harness"], "fake");
    assert_eq!(d["redactions"], 1);
    assert_eq!(d["drafted_at"], before["drafted_at"]);
    assert!(envelope::parse_rfc3339_to_unix(d["edited_at"].as_str().unwrap()).is_some());
    assert_eq!(d.as_object().unwrap().len(), 6, "{d}");
    assert_eq!(edited.as_object().unwrap().len(), 3, "{edited}");
    assert_eq!(
        h.inbox(&qid).unwrap().draft.unwrap(),
        *d,
        "stored draft == printed draft"
    );

    let sent = h.json(&["send", &qid, "--json"]);
    let outbox = h.outbox();
    assert_eq!(outbox.len(), 1);
    let (aid, _) = &outbox[0];
    assert_eq!(
        sent,
        json!({
            "id": aid,
            "in_reply_to": qid,
            "to": fp(&h.maciek),
            "outbox": h.spool().path(Dir::Outbox, aid),
            "redactions": 1,
            "harness": "fake",
        })
    );
    assert!(
        sent["outbox"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/spool/outbox/{aid}.json"))
    );
    assert!(Path::new(sent["outbox"].as_str().unwrap()).is_file());
}

/// `--count --json` counts every unseen record but reports only questions under `questions`.
#[test]
fn count_json_separates_questions_from_answers() {
    let h = Home::new();
    let q = h.put(&h.maciek, "a question?", "pending");
    let asked = question(&h.me, &h.maciek, "what I asked?");
    let reply = Payload::answer(&asked, "the answer", "fake", 0, false);
    let env = Envelope::sign(&reply, &h.maciek);
    let aid = h.put_env(&env, "pending");
    assert_ne!(q, aid);

    assert_eq!(
        h.json(&["inbox", "--count", "--json"]),
        json!({ "count": 2, "questions": 1, "peers": [{ "name": "Maciek", "count": 2 }] })
    );
    assert_eq!(h.ok(&["inbox", "--count"]), "2\n");
    // The claude hook line counts unseen records the same way.
    assert_eq!(
        h.ok(&["inbox", "--count", "--format", "plain"]),
        format!("{SENTENCE_TWO}\n")
    );
}

// ---------------------------------------------------------------- finish(): unlink failure

/// Makes `spool/inbox` read-only (0o555) so `finish` can write `done/` but cannot unlink the
/// inbox file; restores 0o755 on drop, on every path, so the tempdir can be cleaned up.
/// `None` when the chmod does not block writes (running as root): the caller skips.
struct ReadOnlyInbox(std::path::PathBuf);

impl ReadOnlyInbox {
    fn lock(h: &Home) -> Option<ReadOnlyInbox> {
        let dir = h.path().join("spool").join(Dir::Inbox.name());
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let guard = ReadOnlyInbox(dir.clone());
        let probe = dir.join(".write-probe");
        if std::fs::write(&probe, b"x").is_ok() {
            let _ = std::fs::remove_file(&probe);
            eprintln!("skipping: a read-only inbox/ does not block writes here (root?)");
            return None;
        }
        Some(guard)
    }

    fn unlock(self) {}
}

impl Drop for ReadOnlyInbox {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// `inbox/` not writable: `done/` is written (state `answered`) but the inbox file cannot be
/// removed, so `owl send` exits 1 with `removing`; the inbox record is byte-identical. After
/// unlocking, the retry exits 0 with the same answer id, rewrites `done/` in place, removes
/// the original and leaves exactly one outbox envelope.
#[test]
fn send_with_unremovable_inbox_exits_1_and_retries_with_same_answer() {
    let h = Home::new();
    let qid = h.put(&h.maciek, "Where is the retry policy defined?", "pending");
    h.ok(&["draft", &qid]);
    let before = h.inbox(&qid).unwrap();
    let inbox_file = h.spool().path(Dir::Inbox, &qid);
    let bytes_before = std::fs::read(&inbox_file).unwrap();
    let Some(lock) = ReadOnlyInbox::lock(&h) else {
        return;
    };

    let err = h.fails(&["send", &qid], "removing ");
    assert!(err.contains(&format!("inbox/{qid}.json")), "{err}");
    assert_eq!(err.lines().count(), 1, "one clean line: {err}");
    assert_eq!(std::fs::read(&inbox_file).unwrap(), bytes_before);
    assert_eq!(h.inbox(&qid).unwrap(), before);
    let done = h.done(&qid).unwrap();
    assert_eq!(
        done.state, "answered",
        "done/ was written before the unlink failed"
    );
    let outbox = h.outbox();
    assert_eq!(outbox.len(), 1, "{outbox:?}");
    let (aid, arec) = &outbox[0];
    assert_eq!(done.meta["answer_id"], *aid);
    let first_done_at = done.meta["done_at"].clone();
    no_tmp_files(&h, Dir::Done);

    lock.unlock();
    let out = h.ok(&["send", &qid]);
    assert_eq!(
        out,
        format!("sent {aid} (reply to {qid}, to {})\n", fp(&h.maciek)),
        "the retry reuses the answer id from the first attempt"
    );
    assert!(
        h.inbox(&qid).is_none(),
        "the original is gone after the retry"
    );
    let done = h.done(&qid).unwrap();
    assert_eq!(done.state, "answered");
    assert_eq!(done.meta["answer_id"], *aid);
    assert!(done.meta["done_at"].is_string());
    assert_eq!(done.raw, before.raw);
    assert_eq!(done.draft, before.draft);
    let _ = first_done_at; // rewritten in place: same shape, timestamp may or may not differ
    let outbox = h.outbox();
    assert_eq!(outbox.len(), 1, "exactly one envelope: {outbox:?}");
    assert_eq!(outbox[0].1, *arec);
    no_tmp_files(&h, Dir::Done);
    no_tmp_files(&h, Dir::Inbox);
}

/// Same for `owl reject`: exit 1 with `removing`, `done/` holds `rejected` with
/// `previous_state = consent`, inbox byte-identical; the retry keeps `previous_state`.
#[test]
fn reject_with_unremovable_inbox_exits_1_and_retries() {
    let h = Home::new();
    let qid = h.put(&h.maciek, "reject me?", "consent");
    let before = h.inbox(&qid).unwrap();
    let inbox_file = h.spool().path(Dir::Inbox, &qid);
    let bytes_before = std::fs::read(&inbox_file).unwrap();
    let Some(lock) = ReadOnlyInbox::lock(&h) else {
        return;
    };

    let err = h.fails(&["reject", &qid], "removing ");
    assert!(err.contains(&format!("inbox/{qid}.json")), "{err}");
    assert_eq!(err.lines().count(), 1, "one clean line: {err}");
    assert_eq!(std::fs::read(&inbox_file).unwrap(), bytes_before);
    assert_eq!(h.inbox(&qid).unwrap(), before);
    let done = h.done(&qid).unwrap();
    assert_eq!(done.state, "rejected");
    assert_eq!(done.meta["previous_state"], "consent");
    assert!(h.outbox().is_empty());

    lock.unlock();
    assert_eq!(h.ok(&["reject", &qid]), format!("rejected {qid}\n"));
    assert!(h.inbox(&qid).is_none());
    let done = h.done(&qid).unwrap();
    assert_eq!(done.state, "rejected");
    assert_eq!(
        done.meta["previous_state"], "consent",
        "the retry re-reads the untouched original, so previous_state is still consent"
    );
    assert!(done.meta["done_at"].is_string());
    assert!(h.outbox().is_empty());
    no_tmp_files(&h, Dir::Done);
    no_tmp_files(&h, Dir::Inbox);
}

/// Between a failed `send` and its retry the answer is already signed and spooled, so `owl
/// edit` refuses (exit 1, names the outbox envelope, points to `owl send`) instead of
/// accepting an edit the retry would silently drop. The editor never runs; the draft is
/// unchanged; the retry ships the original text.
#[test]
fn edit_after_failed_send_is_refused_because_the_answer_is_spooled() {
    let h = Home::new();
    let qid = h.put(&h.maciek, "Where is the retry policy defined?", "pending");
    h.ok(&["draft", &qid]);
    let before = h.inbox(&qid).unwrap();
    let block = h.spool().path(Dir::Done, &qid);
    std::fs::create_dir(&block).unwrap();
    h.fails(&["send", &qid], &format!("finishing record {qid}"));
    let (aid, arec) = h.outbox().into_iter().next().unwrap();

    let log = h.path().join("editor-ran.txt");
    let script = h.editor_script(&format!("touch {}\necho EDITED >> \"$1\"", log.display()));
    let out = h
        .owl()
        .env("EDITOR", &script)
        .args(["edit", &qid])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains(&format!("outbox/{aid}.json")), "{err}");
    assert!(err.contains(&format!("owl send {qid}")), "{err}");
    assert!(!log.exists(), "the editor must not run");
    assert_eq!(h.inbox(&qid).unwrap(), before, "draft unchanged");
    // --json is refused the same way, with nothing on stdout.
    let out = h
        .owl()
        .env("EDITOR", &script)
        .args(["edit", &qid, "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(!log.exists());

    std::fs::remove_dir(&block).unwrap();
    h.ok(&["send", &qid]);
    let outbox = h.outbox();
    assert_eq!(outbox.len(), 1);
    assert_eq!(
        outbox[0].1, arec,
        "the retry ships the envelope from the first attempt"
    );
    let (text, ..) = answer_body(&payload(&outbox[0].1));
    assert!(!text.contains("EDITED"));
    assert_eq!(text, before.draft.unwrap()["text"]);

    // Negative twin: an ordinary drafted record (no envelope) is still editable.
    let other = h.put(&h.maciek, "editable?", "pending");
    h.ok(&["draft", &other]);
    let out = h
        .owl()
        .env("EDITOR", &script)
        .args(["edit", &other])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(log.exists());
}
