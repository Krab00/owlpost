//! OWL-011 consent and policy tests: `owl allow` / `owl deny`, the `unavailable` answer for
//! denied peers, the daemon's auto-accept path and the outgoing log (design §3.2, §3.4, §5,
//! §8, §9; concept "Trust"). Every test uses its own temp home, `127.0.0.1:0` and the fake
//! harness; `notify` is off in every fixture (`tests/common`).

mod common;

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use common::{
    PATH, PROJECT, Peer, TestDaemon, client, fp, id, policy, post_envelope, prepare_home_with,
    signed, spawn_daemon, spawn_daemon_with,
};
use owlpost::answer::OUTGOING_LOG;
use owlpost::config::{Config, Harness};
use owlpost::contacts::{ContactBook, Mode};
use owlpost::envelope::{self, Body, Envelope, Payload};
use owlpost::identity::{Identity, pubkey_string};
use owlpost::spool::{Dir, Record, Spool};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const OWL: &str = env!("CARGO_BIN_EXE_owl");
const FAKE_HARNESS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/fake-harness.sh"
);
/// Matches the fake harness's one secret line exactly once (a non-default `responder.redact`).
const REDACT_ONE: &str = r"sk-test-\d+";

/// Runs `owl <args>` against `home` from `cwd`; returns (exit code, stdout, stderr).
fn owl(home: &Path, cwd: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(OWL)
        .env_remove("OWLPOST_HOME")
        .env_remove("EDITOR")
        .current_dir(cwd)
        .arg("--home")
        .arg(home)
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

fn owl_ok(home: &Path, cwd: &Path, args: &[&str]) -> String {
    let (code, out, err) = owl(home, cwd, args);
    assert_eq!(code, 0, "owl {args:?} failed: {err}");
    out
}

/// Exit 1 with `needle` on stderr and nothing on stdout.
fn owl_fails(home: &Path, cwd: &Path, args: &[&str], needle: &str) -> String {
    let (code, out, err) = owl(home, cwd, args);
    assert_eq!(code, 1, "owl {args:?}: stdout {out:?} stderr {err:?}");
    assert_eq!(out, "", "owl {args:?} wrote to stdout on failure");
    assert!(
        err.contains(needle),
        "owl {args:?}: stderr {err:?} lacks {needle:?}"
    );
    err
}

/// Spools a question from `from` to `to` into `home`'s inbox in `state`, as the daemon
/// does (§3.2, `meta = {peer, hash}`); returns the id.
fn put(home: &Path, from: &Identity, to: &Identity, text: &str, state: &str) -> String {
    let env = signed(from, to, text);
    let p: Payload = serde_json::from_str(&env.raw).unwrap();
    let hash = match &p.body {
        Body::Question {
            project,
            path,
            question,
        } => envelope::question_hash(project, path, question),
        Body::Answer { .. } => unreachable!(),
    };
    Spool::new(home)
        .unwrap()
        .put(
            Dir::Inbox,
            &p.id,
            &Record {
                raw: env.raw.clone(),
                sig: env.sig.clone(),
                state: state.into(),
                seen: false,
                received_at: envelope::rfc3339_now(),
                draft: None,
                meta: json!({ "peer": p.from, "hash": hash }),
            },
        )
        .unwrap();
    p.id
}

fn inbox(home: &Path, id: &str) -> Option<Record> {
    Spool::new(home).unwrap().get(Dir::Inbox, id).unwrap()
}

fn done(home: &Path, id: &str) -> Option<Record> {
    Spool::new(home).unwrap().get(Dir::Done, id).unwrap()
}

fn outbox(home: &Path) -> Vec<(String, Record)> {
    Spool::new(home)
        .unwrap()
        .list(Dir::Outbox, |_| true)
        .unwrap()
}

fn overlay_path(home: &Path, peer: &Identity) -> std::path::PathBuf {
    home.join("contacts").join(format!("{}.json", fp(peer)))
}

fn overlay(home: &Path, peer: &Identity) -> Value {
    serde_json::from_slice(&std::fs::read(overlay_path(home, peer)).unwrap()).unwrap()
}

fn policy_mode(home: &Path, cwd: &Path, peer: &Identity) -> Option<Mode> {
    ContactBook::load(home, cwd)
        .unwrap()
        .policy_for(&fp(peer))
        .map(|p| p.mode)
}

fn log_lines(home: &Path) -> Vec<Value> {
    std::fs::read_to_string(home.join(OUTGOING_LOG))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn answer_text(p: &Payload) -> (String, String, u32) {
    match &p.body {
        Body::Answer {
            answer,
            harness,
            redactions,
            ..
        } => (answer.clone(), harness.clone(), *redactions),
        Body::Question { .. } => panic!("expected an answer"),
    }
}

/// Polls `cond` every 50 ms for up to `secs` seconds.
fn wait_for(secs: u64, what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Responder config for the CLI-side tests: the fake harness, the project checkout mapped,
/// one redaction pattern.
fn responder_config(cfg: &mut Config, checkout: &Path) {
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
    cfg.responder.redact = vec![REDACT_ONE.into()];
    cfg.projects
        .insert(PROJECT.into(), checkout.to_string_lossy().into_owned());
}

/// A CLI-only home for `me` with local contacts Ana (no policy) and Maciek (manual).
struct Home {
    dir: TempDir,
    checkout: TempDir,
    me: Identity,
    ana: Identity,
    maciek: Identity,
}

impl Home {
    fn new() -> Home {
        let dir = tempfile::tempdir().unwrap();
        let checkout = tempfile::tempdir().unwrap();
        let (me, ana, maciek) = (id(2), id(1), id(3));
        let peers = [
            Peer::new(&ana, "Ana", None),
            Peer::new(&maciek, "Maciek", Some(policy(Mode::Manual, None))),
        ];
        let checkout_path = checkout.path().to_path_buf();
        prepare_home_with(dir.path(), &me, &peers, |cfg| {
            responder_config(cfg, &checkout_path)
        });
        Home {
            dir,
            checkout,
            me,
            ana,
            maciek,
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// The CLI runs from the checkout: no git root, so only local contacts are loaded.
    fn cwd(&self) -> &Path {
        self.checkout.path()
    }

    fn ok(&self, args: &[&str]) -> String {
        owl_ok(self.path(), self.cwd(), args)
    }

    fn fails(&self, args: &[&str], needle: &str) -> String {
        owl_fails(self.path(), self.cwd(), args, needle)
    }

    fn put(&self, from: &Identity, text: &str, state: &str) -> String {
        put(self.path(), from, &self.me, text, state)
    }
}

// ---------------------------------------------------------------- AC1

#[test]
fn allow_once_releases_without_policy() {
    let h = Home::new();
    let ana_fp = fp(&h.ana);
    let held = h.put(&h.ana, "held one?", "consent");
    let held_too = h.put(&h.ana, "held two?", "consent");
    // Negative twins: same state from another peer, another state from the same peer.
    let other_peer = h.put(&h.maciek, "held by maciek?", "consent");
    let already_pending = h.put(&h.ana, "already pending?", "pending");
    let before = std::fs::read(overlay_path(h.path(), &h.ana)).unwrap();
    assert_eq!(policy_mode(h.path(), h.cwd(), &h.ana), None);

    let out = h.ok(&["allow", &ana_fp, "--once"]);
    assert!(out.contains("released 2"), "{out}");
    assert!(out.contains("no policy"), "{out}");
    assert_eq!(inbox(h.path(), &held).unwrap().state, "pending");
    assert_eq!(inbox(h.path(), &held_too).unwrap().state, "pending");
    assert_eq!(inbox(h.path(), &other_peer).unwrap().state, "consent");
    assert_eq!(inbox(h.path(), &already_pending).unwrap().state, "pending");
    assert_eq!(policy_mode(h.path(), h.cwd(), &h.ana), None);
    assert_eq!(
        std::fs::read(overlay_path(h.path(), &h.ana)).unwrap(),
        before,
        "--once writes nothing"
    );
    assert!(overlay(h.path(), &h.ana).get("policy").is_none());

    // Nothing left to release: still exit 0, releases 0; `--json` reports the same.
    let v: Value = serde_json::from_str(&h.ok(&["allow", "ana", "--once", "--json"])).unwrap();
    assert_eq!(v["fingerprint"], ana_fp);
    assert_eq!(v["peer"], "Ana");
    assert_eq!(v["policy"], Value::Null);
    assert_eq!(v["released"], json!([]));
    assert_eq!(policy_mode(h.path(), h.cwd(), &h.ana), None);
    // Unknown peer: exit 1, nothing changes.
    h.fails(&["allow", "nobody", "--once"], "no contact matches");
    assert_eq!(inbox(h.path(), &other_peer).unwrap().state, "consent");
}

// ---------------------------------------------------------------- AC2

#[test]
fn allow_writes_manual_and_always_writes_auto() {
    let h = Home::new();
    let ana_fp = fp(&h.ana);
    let held = h.put(&h.ana, "held?", "consent");

    // Default: manual, and the held question is released.
    let out = h.ok(&["allow", &ana_fp]);
    assert!(
        out.contains("policy manual") && out.contains("released 1"),
        "{out}"
    );
    let file = overlay(h.path(), &h.ana);
    assert_eq!(file["policy"]["mode"], "manual");
    assert_eq!(file["policy"]["scope"]["projects"], json!(["*"]));
    assert_eq!(file["pubkey"], pubkey_string(&h.ana.verifying_key()));
    assert_eq!(file["name"], "Ana", "local contact keeps its fields");
    assert_eq!(policy_mode(h.path(), h.cwd(), &h.ana), Some(Mode::Manual));
    assert_eq!(inbox(h.path(), &held).unwrap().state, "pending");

    // `--always` on a local-source contact without the flag: exit 1, nothing written or released.
    let held = h.put(&h.ana, "held again?", "consent");
    let before = std::fs::read(overlay_path(h.path(), &h.ana)).unwrap();
    let err = h.fails(
        &["allow", &ana_fp, "--always"],
        "--i-verified-the-fingerprint",
    );
    assert!(err.contains(&ana_fp), "{err}");
    assert_eq!(
        std::fs::read(overlay_path(h.path(), &h.ana)).unwrap(),
        before
    );
    assert_eq!(policy_mode(h.path(), h.cwd(), &h.ana), Some(Mode::Manual));
    assert_eq!(inbox(h.path(), &held).unwrap().state, "consent");
    // `--once --always`: exit 1, nothing changes either.
    h.fails(
        &["allow", &ana_fp, "--once", "--always"],
        "mutually exclusive",
    );
    h.fails(
        &[
            "allow",
            &ana_fp,
            "--once",
            "--always",
            "--i-verified-the-fingerprint",
        ],
        "mutually exclusive",
    );
    assert_eq!(
        std::fs::read(overlay_path(h.path(), &h.ana)).unwrap(),
        before
    );
    assert_eq!(inbox(h.path(), &held).unwrap().state, "consent");

    // With the flag: auto, released.
    let out = h.ok(&["allow", "Ana", "--always", "--i-verified-the-fingerprint"]);
    assert!(out.contains("policy auto"), "{out}");
    assert_eq!(overlay(h.path(), &h.ana)["policy"]["mode"], "auto");
    assert_eq!(policy_mode(h.path(), h.cwd(), &h.ana), Some(Mode::Auto));
    assert_eq!(inbox(h.path(), &held).unwrap().state, "pending");

    // A repo-source contact needs no flag: the overlay holds pubkey + policy only.
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(repo.path().join(".git")).unwrap();
    let peers = repo.path().join(".agents").join("peers");
    std::fs::create_dir_all(&peers).unwrap();
    let bea = id(7);
    std::fs::write(
        peers.join("bea.json"),
        serde_json::to_vec_pretty(&json!({
            "name": "Bea",
            "emails": ["bea@example.org"],
            "pubkey": pubkey_string(&bea.verifying_key()),
            "endpoints": [],
        }))
        .unwrap(),
    )
    .unwrap();
    let book = ContactBook::load(h.path(), repo.path()).unwrap();
    assert_eq!(book.resolve("Bea").unwrap().source, "repo");
    let held = h.put(&bea, "from the repo?", "consent");
    let out = owl_ok(h.path(), repo.path(), &["allow", "Bea", "--always"]);
    assert!(out.contains("policy auto"), "{out}");
    let file = overlay(h.path(), &bea);
    assert_eq!(file["policy"]["mode"], "auto");
    assert_eq!(file["pubkey"], pubkey_string(&bea.verifying_key()));
    assert!(file.get("name").is_none(), "overlay only: {file}");
    assert_eq!(policy_mode(h.path(), repo.path(), &bea), Some(Mode::Auto));
    assert_eq!(inbox(h.path(), &held).unwrap().state, "pending");
    // Without the git root the same contact is unknown: the guard is about provenance.
    h.fails(&["allow", "Bea", "--always"], "no contact matches");
}

// ---------------------------------------------------------------- AC3

#[tokio::test]
async fn deny_makes_peer_unavailable() {
    let ana = id(1);
    let maciek = id(3);
    let daemon = spawn_daemon(
        2,
        true,
        &[
            Peer::new(&ana, "Ana", None),
            Peer::new(&maciek, "Maciek", Some(policy(Mode::Manual, None))),
        ],
    )
    .await;
    let home = daemon.home();
    let cwd = tempfile::tempdir().unwrap();
    let held = put(home, &ana, &daemon.id, "held?", "consent");
    let held_too = put(home, &ana, &daemon.id, "held too?", "consent");
    let pending = put(home, &ana, &daemon.id, "already released?", "pending");
    let other = put(home, &maciek, &daemon.id, "maciek held?", "consent");

    let out = owl_ok(home, cwd.path(), &["deny", &fp(&ana)]);
    assert!(
        out.contains("policy never") && out.contains("2 held"),
        "{out}"
    );
    assert_eq!(overlay(home, &ana)["policy"]["mode"], "never");
    for id in [&held, &held_too] {
        assert!(inbox(home, id).is_none(), "{id} left the inbox");
        let d = done(home, id).unwrap();
        assert_eq!(d.state, "denied");
        assert_eq!(d.meta["previous_state"], "consent");
        assert_eq!(d.meta["peer"], fp(&ana), "daemon meta kept");
        assert!(d.meta["done_at"].is_string());
    }
    // Only *held* records of *that* peer move.
    assert_eq!(inbox(home, &pending).unwrap().state, "pending");
    assert_eq!(inbox(home, &other).unwrap().state, "consent");
    assert!(done(home, &other).is_none());

    // The running daemon applies the new policy without a restart: 403 unavailable.
    let cl = client(Some(&ana), &daemon.id);
    let resp = post_envelope(&cl, &daemon, &signed(&ana, &daemon.id, "new question?")).await;
    assert_eq!(resp.status(), 403);
    let denied_headers = resp.headers().clone();
    let denied_body = resp.bytes().await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&denied_body).unwrap(),
        json!({ "error": "unavailable" })
    );
    assert!(
        Spool::new(home)
            .unwrap()
            .list(Dir::Inbox, |r| r.state == "consent")
            .unwrap()
            .iter()
            .all(|(id, _)| *id == other),
        "nothing new was spooled"
    );
    // Negative twin: the manual peer on the same daemon is still served.
    let cl_m = client(Some(&maciek), &daemon.id);
    let resp = post_envelope(&cl_m, &daemon, &signed(&maciek, &daemon.id, "still ok?")).await;
    assert_eq!(resp.status(), 202);

    // Byte-identical to the responder-disabled answer.
    let off = spawn_daemon(4, false, &[Peer::new(&ana, "Ana", None)]).await;
    let cl_off = client(Some(&ana), &off.id);
    let resp = post_envelope(&cl_off, &off, &signed(&ana, &off.id, "new question?")).await;
    assert_eq!(resp.status(), 403);
    assert_eq!(
        resp.headers().get("content-type"),
        denied_headers.get("content-type")
    );
    let off_body = resp.bytes().await.unwrap();
    assert_eq!(denied_body, off_body);
    daemon.running.shutdown();
    off.running.shutdown();
}

// ---------------------------------------------------------------- AC4

/// Daemon B with peer A on `auto`, the fake harness and one redaction pattern.
async fn auto_daemon(seed: u8, ana: &Identity, checkout: Option<&Path>) -> TestDaemon {
    spawn_daemon_with(
        seed,
        &[Peer::new(ana, "Ana", Some(policy(Mode::Auto, None)))],
        |cfg| {
            responder_config(cfg, checkout.unwrap_or(Path::new("/unused")));
            if checkout.is_none() {
                cfg.projects.clear();
            }
        },
    )
    .await
}

#[tokio::test]
async fn auto_accept_answers_without_human() {
    let ana = id(1);
    let checkout = tempfile::tempdir().unwrap();
    let daemon = auto_daemon(2, &ana, Some(checkout.path())).await;
    let home = daemon.home().to_path_buf();
    assert!(
        !Config::load(&home).unwrap().notify,
        "fixture daemons never notify"
    );
    let cl = client(Some(&ana), &daemon.id);
    let q = signed(&ana, &daemon.id, "Where is the retry policy defined?");
    let qid = serde_json::from_str::<Payload>(&q.raw).unwrap().id;
    let resp = post_envelope(&cl, &daemon, &q).await;
    assert_eq!(resp.status(), 202);

    let h = home.clone();
    tokio::task::spawn_blocking(move || {
        wait_for(5, "an outbox record", || !outbox(&h).is_empty());
    })
    .await
    .unwrap();

    let out = outbox(&home);
    assert_eq!(out.len(), 1, "{out:?}");
    let (aid, arec) = &out[0];
    assert_eq!(arec.state, "unacked");
    assert_eq!(arec.meta["question_id"], qid);
    assert_eq!(arec.meta["peer"], fp(&ana));
    let a = Envelope {
        raw: arec.raw.clone(),
        sig: arec.sig.clone(),
    }
    .verify(&daemon.id.verifying_key())
    .expect("answer signed by B");
    assert_eq!(a.id, *aid);
    assert_eq!(a.in_reply_to.as_deref(), Some(qid.as_str()));
    assert_eq!(a.to, fp(&ana));
    assert_eq!(a.from, daemon.fp());
    let (text, harness, redactions) = answer_text(&a);
    assert!(text.contains("[redacted]"), "{text}");
    assert!(!text.contains("sk-test-123456"), "secret leaked: {text}");
    assert_eq!((harness.as_str(), redactions), ("fake", 1));

    let lines = log_lines(&home);
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line = &lines[0];
    assert_eq!(line["mode"], "auto");
    assert_eq!(line["redactions"], 1);
    assert_eq!(line["to"], fp(&ana));
    assert_eq!(line["question_id"], qid);
    assert_eq!(line["harness"], "fake");
    assert_eq!(line["answer_sha256"], sha256_hex(&text));
    assert!(envelope::parse_rfc3339_to_unix(line["ts"].as_str().unwrap()).is_some());
    let keys: Vec<&str> = line
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let mut expected = vec![
        "ts",
        "to",
        "question_id",
        "harness",
        "redactions",
        "answer_sha256",
        "mode",
    ];
    expected.sort_unstable();
    assert_eq!(keys, expected);

    let left = Spool::new(&home)
        .unwrap()
        .list(Dir::Inbox, |r| r.state == "consent" || r.state == "pending")
        .unwrap();
    assert!(left.is_empty(), "{left:?}");
    assert!(inbox(&home, &qid).is_none());
    let d = done(&home, &qid).unwrap();
    assert_eq!(d.state, "answered");
    assert_eq!(d.meta["answer_id"], *aid);
    assert!(d.meta.get("auto_error").is_none());
    // The asker sees the answer on the outbox endpoint.
    let list: Vec<Value> = cl
        .get(daemon.url("/v1/outbox"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list, vec![json!({ "raw": arec.raw, "sig": arec.sig })]);
    daemon.running.shutdown();
}

/// A restart picks up a `pending` auto record with no event behind it (the periodic scan):
/// the record was released by `owl allow --always` while the daemon was down.
#[tokio::test]
async fn auto_accept_scan_picks_up_released_records() {
    let ana = id(1);
    let checkout = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let me = id(2);
    prepare_home_with(dir.path(), &me, &[Peer::new(&ana, "Ana", None)], |cfg| {
        responder_config(cfg, checkout.path())
    });
    let held = put(dir.path(), &ana, &me, "held while down?", "consent");
    let cwd = tempfile::tempdir().unwrap();
    owl_ok(
        dir.path(),
        cwd.path(),
        &[
            "allow",
            &fp(&ana),
            "--always",
            "--i-verified-the-fingerprint",
        ],
    );
    assert_eq!(inbox(dir.path(), &held).unwrap().state, "pending");
    assert!(outbox(dir.path()).is_empty(), "the CLI never answers");

    let daemon = common::respawn(dir, me).await;
    let home = daemon.home().to_path_buf();
    let h = home.clone();
    tokio::task::spawn_blocking(move || {
        wait_for(5, "the scan to answer", || !outbox(&h).is_empty());
    })
    .await
    .unwrap();
    assert_eq!(done(&home, &held).unwrap().state, "answered");
    assert_eq!(log_lines(&home).len(), 1);
    assert_eq!(log_lines(&home)[0]["mode"], "auto");
    daemon.running.shutdown();
}

// ---------------------------------------------------------------- AC5

#[tokio::test]
async fn auto_accept_failure_is_visible() {
    let ana = id(1);
    let daemon = auto_daemon(2, &ana, None).await;
    let home = daemon.home().to_path_buf();
    let cl = client(Some(&ana), &daemon.id);
    let q = signed(&ana, &daemon.id, "Where is the retry policy defined?");
    let qid = serde_json::from_str::<Payload>(&q.raw).unwrap().id;
    assert_eq!(post_envelope(&cl, &daemon, &q).await.status(), 202);

    let (h, i) = (home.clone(), qid.clone());
    tokio::task::spawn_blocking(move || {
        wait_for(5, "auto_error", || {
            inbox(&h, &i).is_some_and(|r| r.meta.get("auto_error").is_some())
        });
    })
    .await
    .unwrap();
    let rec = inbox(&home, &qid).unwrap();
    assert_eq!(rec.state, "pending");
    assert!(rec.draft.is_none());
    let err = rec.meta["auto_error"].as_str().unwrap();
    assert!(err.contains("unknown project"), "{err}");
    assert!(err.contains(PROJECT), "{err}");
    assert_eq!(rec.meta["peer"], fp(&ana), "daemon meta kept");
    assert!(outbox(&home).is_empty());
    assert!(log_lines(&home).is_empty());
    assert!(done(&home, &qid).is_none());

    let cwd = tempfile::tempdir().unwrap();
    let out = owl_ok(&home, cwd.path(), &["inbox"]);
    assert!(out.contains(&qid), "{out}");
    assert!(out.contains("unknown project"), "{out}");
    assert!(out.contains("auto-accept failed"), "{out}");
    assert!(out.contains("Ana"), "{out}");
    let rows: Value =
        serde_json::from_str(&owl_ok(&home, cwd.path(), &["inbox", "--json"])).unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == qid)
        .unwrap();
    assert!(
        row["auto_error"]
            .as_str()
            .unwrap()
            .contains("unknown project")
    );
    assert_eq!(row["state"], "pending");
    // The record stays for the human; the scan does not hammer it (still exactly one error,
    // nothing sent, after another interval's worth of time is not needed: the failed record
    // is excluded from the candidate set by design — asserted in `auto.rs` unit tests).
    daemon.running.shutdown();
}

// ---------------------------------------------------------------- AC6

#[test]
fn manual_send_logs_outgoing() {
    let h = Home::new();
    let qid = h.put(&h.maciek, "Where is the retry policy defined?", "pending");
    h.ok(&["draft", &qid]);
    assert!(
        !h.path().join(OUTGOING_LOG).exists(),
        "drafting logs nothing"
    );
    let draft_text = inbox(h.path(), &qid).unwrap().draft.unwrap()["text"]
        .as_str()
        .unwrap()
        .to_string();
    h.ok(&["send", &qid]);
    let lines = log_lines(h.path());
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line = &lines[0];
    assert_eq!(line["mode"], "manual");
    assert_eq!(line["question_id"], qid);
    assert_eq!(line["to"], fp(&h.maciek));
    assert_eq!(line["harness"], "fake");
    assert_eq!(line["redactions"], 1);
    assert_eq!(line["answer_sha256"], sha256_hex(&draft_text));
    assert!(envelope::parse_rfc3339_to_unix(line["ts"].as_str().unwrap()).is_some());
    let (sent, _, _) = answer_text(&serde_json::from_str(&outbox(h.path())[0].1.raw).unwrap());
    assert_eq!(
        line["answer_sha256"],
        sha256_hex(&sent),
        "hash of the text as sent"
    );

    // A second exchange appends a second line; the first is untouched.
    let q2 = h.put(&h.maciek, "second?", "pending");
    h.ok(&["draft", &q2]);
    h.ok(&["send", &q2]);
    let lines = log_lines(h.path());
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], *line);
    assert_eq!(lines[1]["question_id"], q2);
    assert_eq!(lines[1]["mode"], "manual");
    // A failed send (no draft) logs nothing.
    let q3 = h.put(&h.maciek, "third?", "pending");
    h.fails(&["send", &q3], "has no draft");
    assert_eq!(log_lines(h.path()).len(), 2);
}

// ---------------------------------------------------------------- AC7

#[test]
fn inbox_prompts_for_consent_records() {
    let h = Home::new();
    let ana_fp = fp(&h.ana);
    let held = h.put(&h.ana, "may I?", "consent");
    // Negative twin: same peer, released already — no prompt for it.
    let pending = h.put(&h.maciek, "go ahead?", "pending");

    let out = h.ok(&["inbox"]);
    assert!(out.contains(&held) && out.contains(&pending), "{out}");
    assert!(
        out.contains(&format!("owl allow {ana_fp}")),
        "allow with the fingerprint: {out}"
    );
    assert!(
        out.contains(&format!("owl deny {ana_fp}")),
        "deny with the fingerprint: {out}"
    );
    assert!(
        out.contains(&format!(
            "Ana wants to ask your agent about {PROJECT} — owl allow {ana_fp} [--once|--always] / owl deny {ana_fp}"
        )),
        "{out}"
    );
    assert!(
        !out.contains(&format!("owl allow {}", fp(&h.maciek))),
        "no prompt for a released record: {out}"
    );
    assert_eq!(
        out.matches("wants to ask your agent").count(),
        1,
        "one prompt per held record: {out}"
    );
    assert!(!out.contains("auto-accept failed"), "{out}");
    let rows: Value = serde_json::from_str(&h.ok(&["inbox", "--json"])).unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == held)
        .unwrap();
    assert_eq!(row["state"], "consent");
    assert_eq!(row["auto_error"], Value::Null);

    // Once allowed, the prompt disappears; once denied, the record is gone.
    h.ok(&["allow", &ana_fp]);
    let out = h.ok(&["inbox"]);
    assert!(!out.contains("wants to ask"), "{out}");
    let held = h.put(&h.ana, "again?", "consent");
    let out = h.ok(&["inbox"]);
    assert!(out.contains("wants to ask"), "{out}");
    h.ok(&["deny", &ana_fp]);
    let out = h.ok(&["inbox"]);
    assert!(
        !out.contains("wants to ask") && !out.contains(&held),
        "{out}"
    );
    assert_eq!(done(h.path(), &held).unwrap().state, "denied");
    assert!(out.contains(&pending) && out.contains(PATH), "{out}");
}
