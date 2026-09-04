//! OWL-007: `owl ask` as a subprocess against in-process daemons (AC1..AC7).
//!
//! A (seed 1) asks, B (seed 2) answers. Every test has its own temp homes; every daemon
//! binds 127.0.0.1:0; a "closed port" is one a `TcpListener` bound to and released.

mod common;

use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path as AxPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_server::Handle;
use axum_server::tls_rustls::{RustlsAcceptor, RustlsConfig};
use common::*;
use owlpost::client::{self, SendOutcome};
use owlpost::contacts::{ContactBook, Mode};
use owlpost::envelope::{self, Body, Envelope, Kind, Payload, question_hash};
use owlpost::identity::Identity;
use owlpost::spool::{Dir, Record, Spool};
use owlpost::tls;
use serde_json::{Value, json};
use tempfile::TempDir;

const QUESTION: &str = "Why is the refresh token rotated on every read?";
const ANSWER: &str = "Because the session store is append-only.";

/// `host:port` nobody listens on (bound, read, released).
fn closed_port() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().to_string()
}

/// A's home: key, config, and a contact `Bea` for `b` reachable at `endpoints`.
fn asker_home(a: &Identity, b: &Identity, endpoints: &[&str]) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    prepare_home(dir.path(), a, true, &[]);
    write_contact_full(
        dir.path(),
        &Peer::new(b, "Bea", None),
        endpoints,
        &["bea@example.org"],
    );
    dir
}

fn manual() -> Option<owlpost::contacts::Policy> {
    Some(policy(Mode::Manual, None))
}

fn owl(home: &Path, cwd: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_owl"));
    c.env_remove("OWLPOST_HOME")
        .arg("--home")
        .arg(home)
        .current_dir(cwd)
        .stdin(Stdio::null());
    c
}

/// `owl ask Bea <PATH> <QUESTION> --project <PROJECT> <extra…>` run from `cwd`.
fn ask_with(home: &Path, cwd: &Path, question: &str, extra: &[&str]) -> Output {
    owl(home, cwd)
        .args(["ask", "Bea", PATH, question, "--project", PROJECT])
        .args(extra)
        .output()
        .unwrap()
}

fn ask(home: &Path, extra: &[&str]) -> Output {
    ask_with(home, home, QUESTION, extra)
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn ids(spool: &Spool, dir: Dir) -> Vec<String> {
    spool
        .list(dir, |_| true)
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

fn accepted_id(out: &Output) -> String {
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(out));
    let line = stdout(out);
    line.trim()
        .strip_prefix("accepted ")
        .unwrap_or_else(|| panic!("stdout {line:?}"))
        .to_string()
}

fn payload(rec: &Record) -> Payload {
    serde_json::from_str(&rec.raw).unwrap()
}

fn answer_text(p: &Payload) -> &str {
    match &p.body {
        Body::Answer { answer, .. } => answer,
        Body::Question { .. } => panic!("not an answer: {p:?}"),
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Fresh repo with `f.txt`: one commit per `(email, lines)` appending that many lines.
fn fixture_repo(authors: &[(&str, usize)]) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q"]);
    let file = repo.join("f.txt");
    let mut content = String::new();
    for (i, (email, n)) in authors.iter().enumerate() {
        for k in 0..*n {
            content.push_str(&format!("line {i}-{k} by {email}\n"));
        }
        std::fs::write(&file, &content).unwrap();
        git(repo, &["add", "f.txt"]);
        git(
            repo,
            &[
                "-c",
                &format!("user.email={email}"),
                "-c",
                "user.name=Someone",
                "commit",
                "-q",
                "-m",
                &format!("commit {i}"),
            ],
        );
    }
    dir
}

/// Ask from A's home, where the `--project` flag is left out so detection runs in `cwd`.
fn ask_no_project(home: &Path, cwd: &Path, question: &str) -> Output {
    owl(home, cwd)
        .args(["ask", "Bea", PATH, question])
        .output()
        .unwrap()
}

// ---- scripted fake peer ------------------------------------------------------------------
// Speaks B's side of §7 over B's pinned mTLS, but every response is scripted: what the real
// daemon never sends (a malformed `202`, a failing outbox, a refused ack) has to come from here.

#[derive(Default)]
struct FakeScript {
    /// Verbatim `(status, body)` for `POST /v1/questions`; `None` = a well-formed `202`.
    accept: Option<(u16, String)>,
    /// Extra response headers sent with the scripted `accept` row (`Retry-After`,
    /// `X-Owl-Signature`); the daemon never omits either, so their absence is scripted here.
    accept_headers: Vec<(String, String)>,
    /// `GET /v1/outbox` answers `500` until this instant.
    outbox_fails_until: Option<Instant>,
    /// Status of `POST /v1/outbox/{id}/ack` (`204` is what the daemon sends).
    ack_status: u16,
    /// When set, every accepted question is answered with this text straight into the outbox.
    answer_with: Option<String>,
}

struct FakeState {
    id: Identity,
    script: FakeScript,
    questions: Mutex<Vec<Payload>>,
    outbox: Mutex<Vec<(Payload, Envelope)>>,
    acked: Mutex<Vec<String>>,
}

struct FakePeer {
    addr: SocketAddr,
    state: Arc<FakeState>,
    handle: Handle<SocketAddr>,
}

impl FakePeer {
    fn questions(&self) -> Vec<Payload> {
        self.state.questions.lock().unwrap().clone()
    }
    fn answers(&self) -> Vec<Payload> {
        self.state
            .outbox
            .lock()
            .unwrap()
            .iter()
            .map(|(p, _)| p.clone())
            .collect()
    }
    fn acked(&self) -> Vec<String> {
        self.state.acked.lock().unwrap().clone()
    }
    fn shutdown(&self) {
        self.handle.shutdown();
    }
}

async fn fake_questions(
    State(st): State<Arc<FakeState>>,
    body: String,
) -> (StatusCode, HeaderMap, String) {
    let q: Payload = serde_json::from_str(&body).expect("client sends a JSON payload");
    st.questions.lock().unwrap().push(q.clone());
    let mut headers = HeaderMap::new();
    headers.insert("content-type", "application/json".parse().unwrap());
    if let Some((status, body)) = &st.script.accept {
        for (k, v) in &st.script.accept_headers {
            headers.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        return (
            StatusCode::from_u16(*status).unwrap(),
            headers,
            body.clone(),
        );
    }
    if let Some(text) = &st.script.answer_with {
        let ans = Payload::answer(&q, text, "fake", 0, false);
        let env = Envelope::sign(&ans, &st.id);
        st.outbox.lock().unwrap().push((ans, env));
    }
    (
        StatusCode::ACCEPTED,
        headers,
        json!({ "status": "accepted", "id": q.id }).to_string(),
    )
}

async fn fake_outbox(State(st): State<Arc<FakeState>>) -> (StatusCode, Json<Value>) {
    if st
        .script
        .outbox_fails_until
        .is_some_and(|until| Instant::now() < until)
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "still starting" })),
        );
    }
    let acked = st.acked.lock().unwrap();
    let items: Vec<Envelope> = st
        .outbox
        .lock()
        .unwrap()
        .iter()
        .filter(|(p, _)| !acked.contains(&p.id))
        .map(|(_, e)| e.clone())
        .collect();
    (StatusCode::OK, Json(serde_json::to_value(items).unwrap()))
}

async fn fake_ack(
    State(st): State<Arc<FakeState>>,
    AxPath(id): AxPath<String>,
) -> (StatusCode, Json<Value>) {
    let status = StatusCode::from_u16(st.script.ack_status).unwrap();
    if status == StatusCode::NO_CONTENT {
        st.acked.lock().unwrap().push(id);
        return (status, Json(Value::Null));
    }
    (status, Json(json!({ "error": "ack refused by script" })))
}

/// A fake peer with B's identity (`seed`) that admits only `asker`'s key.
async fn spawn_fake(seed: u8, asker: &Identity, script: FakeScript) -> FakePeer {
    let id = id(seed);
    let allowed = [key(asker)].into_iter().collect();
    let cfg = tls::server_config(&id, allowed, false).unwrap();
    let state = Arc::new(FakeState {
        id,
        script,
        questions: Mutex::new(vec![]),
        outbox: Mutex::new(vec![]),
        acked: Mutex::new(vec![]),
    });
    let app = Router::new()
        .route("/v1/questions", post(fake_questions))
        .route("/v1/outbox", get(fake_outbox))
        .route("/v1/outbox/{id}/ack", post(fake_ack))
        .with_state(state.clone());
    let handle = Handle::new();
    let server = axum_server::bind("127.0.0.1:0".parse().unwrap())
        .acceptor(RustlsAcceptor::new(RustlsConfig::from_config(cfg)))
        .handle(handle.clone());
    tokio::spawn(server.serve(app.into_make_service()));
    let addr = handle.listening().await.expect("fake peer bound");
    FakePeer {
        addr,
        state,
        handle,
    }
}

fn well_formed() -> FakeScript {
    FakeScript {
        ack_status: 204,
        answer_with: Some(ANSWER.into()),
        ..Default::default()
    }
}

// AC1
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ask_accepted_writes_ask_record() {
    let a = id(1);
    let b = spawn_daemon(2, true, &[Peer::new(&a, "Ana", manual())]).await;
    let home = asker_home(&a, &b.id, &[&b.addr.to_string()]);
    let out = ask(home.path(), &[]);
    let qid = accepted_id(&out);
    assert!(stderr(&out).is_empty(), "stderr: {}", stderr(&out));

    let spool = Spool::new(home.path()).unwrap();
    let rec = spool.get(Dir::Asks, &qid).unwrap().expect("asks/<id>.json");
    assert_eq!(rec.state, "waiting");
    assert!(!rec.seen);
    assert_eq!(rec.meta["peer"], fp(&b.id));
    assert_eq!(
        rec.meta["hash"],
        question_hash(PROJECT, Some(PATH), QUESTION)
    );
    // The stored raw/sig is the exact signed question, verifiable with A's key.
    let q = Envelope {
        raw: rec.raw.clone(),
        sig: rec.sig.clone(),
    }
    .verify(&a.verifying_key())
    .unwrap();
    assert_eq!(q.id, qid);
    assert_eq!(q.kind, Kind::Question);
    assert_eq!(q.from, fp(&a));
    assert_eq!(q.to, fp(&b.id));
    assert_eq!(
        q.body,
        Body::Question {
            project: PROJECT.into(),
            path: Some(PATH.into()),
            question: QUESTION.into()
        }
    );
    assert_eq!(ids(&spool, Dir::Asks), std::slice::from_ref(&qid));
    assert!(ids(&spool, Dir::Inbox).is_empty());
    assert!(ids(&spool, Dir::Cache).is_empty());
    assert!(ids(&spool, Dir::Done).is_empty());
    // B spooled the very same question.
    let b_rec = b
        .spool()
        .get(Dir::Inbox, &qid)
        .unwrap()
        .expect("in B's inbox");
    assert_eq!(b_rec.state, "pending");
    assert_eq!(b_rec.raw, rec.raw);

    // --json: machine shape, and a second ask (fresh id) lands next to the first.
    let out = ask(home.path(), &["--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(v.get("status").and_then(Value::as_str), Some("accepted"));
    let qid2 = v.get("id").and_then(Value::as_str).unwrap().to_string();
    assert_ne!(qid2, qid);
    assert_eq!(ids(&spool, Dir::Asks).len(), 2);
    b.running.shutdown();
}

/// Sends `QUESTION` to a fake peer whose `POST /v1/questions` reply is scripted verbatim.
async fn ask_scripted_202(
    a: &Identity,
    status: u16,
    body: &str,
) -> (Output, FakePeer, Spool, TempDir) {
    let peer = spawn_fake(
        2,
        a,
        FakeScript {
            accept: Some((status, body.to_string())),
            ack_status: 204,
            ..Default::default()
        },
    )
    .await;
    let home = asker_home(a, &peer.state.id, &[&peer.addr.to_string()]);
    let out = ask(home.path(), &[]);
    let spool = Spool::new(home.path()).unwrap();
    (out, peer, spool, home)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn accepted_202_with_a_bad_body_is_a_clean_error() {
    let a = id(1);
    // Well-formed twin through the same fake: the id echoes the question id, ask recorded.
    let peer = spawn_fake(
        2,
        &a,
        FakeScript {
            ack_status: 204,
            ..Default::default()
        },
    )
    .await;
    let home = asker_home(&a, &peer.state.id, &[&peer.addr.to_string()]);
    let qid = accepted_id(&ask(home.path(), &[]));
    assert_eq!(peer.questions()[0].id, qid);
    assert_eq!(ids(&Spool::new(home.path()).unwrap(), Dir::Asks), [qid]);
    peer.shutdown();

    // Every malformed 202 body: exit 1 (never a panic), the reason named, nothing recorded.
    for (body, reason) in [
        ("[]", "202 body has no id"),
        ("\"accepted\"", "202 body has no id"),
        ("null", "202 body has no id"),
        (r#"{"status":"accepted"}"#, "202 body has no id"),
        (r#"{"status":"accepted","id":7}"#, "202 body has no id"),
        ("accepted", "202 body is not JSON"),
        ("", "202 body is not JSON"),
    ] {
        let (out, peer, spool, _home) = ask_scripted_202(&a, 202, body).await;
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(1), "body {body:?}: {err}");
        assert!(err.contains(reason), "body {body:?}: {err}");
        assert!(!err.contains("panicked"), "body {body:?}: {err}");
        assert!(stdout(&out).is_empty(), "body {body:?}");
        assert_eq!(peer.questions().len(), 1, "the question was sent");
        for dir in Dir::ALL {
            assert!(
                ids(&spool, dir).is_empty(),
                "body {body:?}: {} must stay empty",
                dir.name()
            );
        }
        peer.shutdown();
    }

    // A 202 that accepts some OTHER id is refused: the asks/ record would never be answered.
    let (out, peer, spool, _home) =
        ask_scripted_202(&a, 202, r#"{"status":"accepted","id":"someone-elses-id"}"#).await;
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{err}");
    let sent = &peer.questions()[0].id;
    assert!(
        err.contains(&format!(
            "peer accepted id someone-elses-id but the question id is {sent}"
        )),
        "{err}"
    );
    assert!(stdout(&out).is_empty());
    assert!(
        ids(&spool, Dir::Asks).is_empty(),
        "no asks/ record for a mismatched id"
    );
    peer.shutdown();
}

// AC2
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cache_hit_skips_network() {
    let (a, b) = (id(1), id(2));
    let home = asker_home(&a, &b, &[&closed_port()]);
    let spool = Spool::new(home.path()).unwrap();
    let hash = question_hash(PROJECT, Some(PATH), QUESTION);
    let q = Payload::question(&fp(&a), &fp(&b), PROJECT, Some(PATH), QUESTION);
    let env = Envelope::sign(&Payload::answer(&q, "Cached answer.", "fake", 0, true), &b);
    spool.cache_put(&hash, &record(&env, "pending")).unwrap();

    let out = ask(home.path(), &[]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "Cached answer.");
    assert!(
        ids(&spool, Dir::Asks).is_empty(),
        "nothing sent, nothing queued"
    );
    assert!(ids(&spool, Dir::Inbox).is_empty());

    // The hash normalises whitespace and case, so this is the same entry.
    let out = ask_with(
        home.path(),
        home.path(),
        "  WHY is   the refresh token rotated on every READ? ",
        &[],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "Cached answer.");

    // --json prints the cached payload itself.
    let out = ask(home.path(), &["--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(v.get("type").and_then(Value::as_str), Some("answer"));
    assert_eq!(
        v.get("body")
            .and_then(|b| b.get("answer"))
            .and_then(Value::as_str),
        Some("Cached answer.")
    );

    // A different question misses the cache and reaches the (dead) endpoint.
    let out = ask_with(home.path(), home.path(), "Something else?", &[]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("offline"), "{}", stderr(&out));

    // --no-cache bypasses the populated entry: the send is attempted and fails offline.
    let out = ask(home.path(), &["--no-cache"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("offline"), "{}", stderr(&out));
    assert!(ids(&spool, Dir::Asks).is_empty());
}

/// A cache entry that is not a verified answer is a miss: the send is attempted (here: to a
/// closed port, so exit 2 offline), and the corrupt entry is warned about, never printed.
#[test]
fn corrupt_cache_entries_fall_through() {
    let (a, b) = (id(1), id(2));
    let hash = question_hash(PROJECT, Some(PATH), QUESTION);
    let q = Payload::question(&fp(&a), &fp(&b), PROJECT, Some(PATH), QUESTION);
    let question_env = Envelope::sign(&q, &a);
    let answer_env = Envelope::sign(&Payload::answer(&q, "Cached answer.", "fake", 0, true), &b);
    let mut junk_raw = record(&answer_env, "pending");
    junk_raw.raw = "{not json".into();
    let mut object_not_payload = record(&answer_env, "pending");
    object_not_payload.raw = r#"{"status":"pending"}"#.into();
    for (what, rec) in [
        // Positive twin's exact shape, but kind `question`: the one dimension that differs.
        ("a question payload", record(&question_env, "pending")),
        ("unparseable raw", junk_raw),
        ("an object that is not a payload", object_not_payload),
    ] {
        let home = asker_home(&a, &b, &[&closed_port()]);
        let spool = Spool::new(home.path()).unwrap();
        spool.cache_put(&hash, &rec).unwrap();
        let out = ask(home.path(), &[]);
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(2), "{what}: {err}");
        assert!(
            err.contains("ignoring corrupt cache entry"),
            "{what}: {err}"
        );
        assert!(err.contains("offline"), "{what}: {err}");
        assert!(
            stdout(&out).is_empty(),
            "{what}: nothing is printed from the cache"
        );
        assert!(!err.contains("panicked"), "{what}: {err}");
        assert!(ids(&spool, Dir::Asks).is_empty(), "{what}");
        // --quiet drops the warning, not the miss.
        let out = ask(home.path(), &["--quiet"]);
        assert_eq!(out.status.code(), Some(2), "{what}: {}", stderr(&out));
        assert!(
            !stderr(&out).contains("ignoring"),
            "{what}: {}",
            stderr(&out)
        );
    }
    // The twin with kind `answer` and the same closed port is served from the cache.
    let home = asker_home(&a, &b, &[&closed_port()]);
    let spool = Spool::new(home.path()).unwrap();
    spool
        .cache_put(&hash, &record(&answer_env, "pending"))
        .unwrap();
    let out = ask(home.path(), &[]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "Cached answer.");
    assert!(stderr(&out).is_empty(), "{}", stderr(&out));
}

/// A cache record that cannot be read at all — not a record with bad contents, but a file the
/// spool fails to load (a directory in its place, or bytes that are not a record) — is also a
/// miss: warned as `unreadable`, then the send is attempted (closed port → exit 2 offline).
/// Twin of `corrupt_cache_entries_fall_through`, which reaches the loadable-but-wrong arm.
#[test]
fn unreadable_cache_entries_fall_through() {
    let (a, b) = (id(1), id(2));
    let hash = question_hash(PROJECT, Some(PATH), QUESTION);
    for what in ["a directory", "bytes that are not a record"] {
        let home = asker_home(&a, &b, &[&closed_port()]);
        let spool = Spool::new(home.path()).unwrap();
        let path = spool.path(Dir::Cache, &hash);
        if what == "a directory" {
            std::fs::create_dir(&path).unwrap();
        } else {
            std::fs::write(&path, b"{\"raw\": \"tru").unwrap();
        }
        assert!(
            spool.cache_get(&hash).is_err(),
            "{what}: the spool itself fails to read the entry"
        );
        let out = ask(home.path(), &[]);
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(2), "{what}: {err}");
        assert!(
            err.contains("ignoring unreadable cache entry"),
            "{what}: {err}"
        );
        assert!(!err.contains("corrupt"), "{what}: the other arm: {err}");
        assert!(err.contains("offline"), "{what}: {err}");
        assert!(
            stdout(&out).is_empty(),
            "{what}: nothing printed from the cache"
        );
        assert!(!err.contains("panicked"), "{what}: {err}");
        assert!(ids(&spool, Dir::Asks).is_empty(), "{what}");
        // --quiet drops the warning, not the miss.
        let out = ask(home.path(), &["--quiet"]);
        assert_eq!(out.status.code(), Some(2), "{what}: {}", stderr(&out));
        assert!(
            !stderr(&out).contains("ignoring"),
            "{what}: {}",
            stderr(&out)
        );
        assert!(ids(&spool, Dir::Asks).is_empty(), "{what}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_cache_sends_to_a_live_peer_despite_a_hit() {
    let a = id(1);
    let b = spawn_daemon(2, true, &[Peer::new(&a, "Ana", manual())]).await;
    let home = asker_home(&a, &b.id, &[&b.addr.to_string()]);
    let spool = Spool::new(home.path()).unwrap();
    let hash = question_hash(PROJECT, Some(PATH), QUESTION);
    let q = Payload::question(&fp(&a), &fp(&b.id), PROJECT, Some(PATH), QUESTION);
    let env = Envelope::sign(
        &Payload::answer(&q, "Cached answer.", "fake", 0, true),
        &b.id,
    );
    spool.cache_put(&hash, &record(&env, "pending")).unwrap();

    let out = ask(home.path(), &["--no-cache"]);
    let qid = accepted_id(&out);
    assert!(b.spool().get(Dir::Inbox, &qid).unwrap().is_some());
    assert_eq!(ids(&spool, Dir::Asks), [qid]);
    // The cache entry itself is untouched.
    assert_eq!(spool.cache_get(&hash).unwrap().unwrap().raw, env.raw);
    b.running.shutdown();
}

// AC3
#[test]
fn offline_exits_2_and_queues_nothing() {
    let (a, b) = (id(1), id(2));
    let port = closed_port();
    let home = asker_home(&a, &b, &[&port]);
    let out = ask(home.path(), &[]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("offline"), "{err}");
    assert!(err.contains(&port), "names the endpoint: {err}");
    assert!(stdout(&out).is_empty());
    let spool = Spool::new(home.path()).unwrap();
    for dir in Dir::ALL {
        assert!(
            ids(&spool, dir).is_empty(),
            "{} must stay empty",
            dir.name()
        );
    }

    // A contact with no endpoints at all is valid since OWL-017 (iroh reaches it); with no
    // local daemon to forward over iroh it is offline (2), naming the missing daemon.
    let home = asker_home(&a, &b, &[]);
    let out = ask(home.path(), &[]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert_eq!(
        stderr(&out).trim(),
        "owl: offline: no endpoint of Bea reachable (iroh: no local daemon (daemon.addr missing))"
    );
    assert!(stdout(&out).is_empty());

    // Unknown peer: resolution fails before anything is sent (1).
    let out = owl(home.path(), home.path())
        .args(["ask", "Zed", PATH, QUESTION, "--project", PROJECT])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("no contact matches"),
        "{}",
        stderr(&out)
    );
}

// AC4
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn endpoints_are_tried_in_order() {
    let a = id(1);
    // Two daemons with B's identity, so either one is a valid pinned peer: the question
    // must land in the FIRST reachable one.
    let first = spawn_daemon(2, true, &[Peer::new(&a, "Ana", manual())]).await;
    let second = spawn_daemon(2, true, &[Peer::new(&a, "Ana", manual())]).await;
    let dead = closed_port();
    let (f, s) = (first.addr.to_string(), second.addr.to_string());

    let home = asker_home(&a, &first.id, &[&dead, &f, &s]);
    let qid = accepted_id(&ask(home.path(), &[]));
    assert!(first.spool().get(Dir::Inbox, &qid).unwrap().is_some());
    assert!(second.spool().get(Dir::Inbox, &qid).unwrap().is_none());
    assert_eq!(ids(&Spool::new(home.path()).unwrap(), Dir::Asks), [qid]);

    let home = asker_home(&a, &first.id, &[&dead, &s, &f]);
    let qid = accepted_id(&ask(home.path(), &[]));
    assert!(second.spool().get(Dir::Inbox, &qid).unwrap().is_some());
    assert!(first.spool().get(Dir::Inbox, &qid).unwrap().is_none());

    // Two dead endpoints ahead of the live one still get there.
    let home = asker_home(&a, &first.id, &[&dead, &closed_port(), &f]);
    let qid = accepted_id(&ask(home.path(), &[]));
    assert!(first.spool().get(Dir::Inbox, &qid).unwrap().is_some());
    first.running.shutdown();
    second.running.shutdown();
}

/// Waits for the first inbox record on B, then after `delay` writes B's signed answer to
/// its outbox (state `unacked`) — what `owl send` / the auto-accept scheduler will do.
fn answer_later(
    b_home: PathBuf,
    b_id: Identity,
    delay: Duration,
) -> std::thread::JoinHandle<(String, String)> {
    std::thread::spawn(move || {
        let spool = Spool::new(&b_home).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let (qid, rec) = loop {
            if let Some(x) = spool.list(Dir::Inbox, |_| true).unwrap().into_iter().next() {
                break x;
            }
            assert!(Instant::now() < deadline, "no question reached B");
            std::thread::sleep(Duration::from_millis(20));
        };
        std::thread::sleep(delay);
        let q = payload(&rec);
        let ans = Payload::answer(&q, ANSWER, "fake", 0, false);
        let env = Envelope::sign(&ans, &b_id);
        spool
            .put(Dir::Outbox, &ans.id, &record(&env, "unacked"))
            .unwrap();
        (qid, ans.id)
    })
}

// AC5
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_returns_answer() {
    let a = id(1);
    let b = spawn_daemon(2, true, &[Peer::new(&a, "Ana", manual())]).await;
    let home = asker_home(&a, &b.id, &[&b.addr.to_string()]);
    let answerer = answer_later(
        b.home().to_path_buf(),
        Identity::from_seed([2; 32]),
        Duration::from_secs(1),
    );
    let started = Instant::now();
    let out = ask(home.path(), &["--wait", "10"]);
    let (qid, aid) = answerer.join().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), ANSWER);
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "returned as soon as the answer appeared, not at the deadline"
    );
    assert!(
        stderr(&out).contains(&format!("accepted {qid}")),
        "{}",
        stderr(&out)
    );
    assert!(
        !stderr(&out).contains("warning"),
        "a 204 ack and a clean poll must not warn: {}",
        stderr(&out)
    );

    let spool = Spool::new(home.path()).unwrap();
    let hash = question_hash(PROJECT, Some(PATH), QUESTION);
    let inbox = spool
        .get(Dir::Inbox, &aid)
        .unwrap()
        .expect("answer in A's inbox");
    assert_eq!(inbox.state, "pending");
    assert!(!inbox.seen);
    let p = payload(&inbox);
    assert_eq!(p.kind, Kind::Answer);
    assert_eq!(p.in_reply_to.as_deref(), Some(qid.as_str()));
    assert_eq!(answer_text(&p), ANSWER);
    assert_eq!(inbox.meta["peer"], fp(&b.id));
    assert_eq!(inbox.meta["in_reply_to"], qid);
    let cached = spool.cache_get(&hash).unwrap().expect("answer cached");
    assert_eq!(cached.raw, inbox.raw);
    assert_eq!(cached.sig, inbox.sig);
    assert!(
        spool.get(Dir::Asks, &qid).unwrap().is_none(),
        "ask left asks/"
    );
    assert_eq!(
        spool.get(Dir::Done, &qid).unwrap().unwrap().state,
        "answered"
    );
    // B: acked → done/.
    let bs = b.spool();
    assert!(bs.get(Dir::Outbox, &aid).unwrap().is_none());
    assert_eq!(bs.get(Dir::Done, &aid).unwrap().unwrap().state, "acked");

    // The same question now hits A's cache without a send.
    b.running.shutdown();
    let out = ask(home.path(), &[]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), ANSWER);
}

/// `--wait` runs the daemon's ingestion: a forged entry ahead of the genuine answer in B's
/// outbox is skipped and never acked; the genuine one is taken.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_skips_forged_entries_and_takes_the_genuine_answer() {
    let a = id(1);
    let b = spawn_daemon(2, true, &[Peer::new(&a, "Ana", manual())]).await;
    let home = asker_home(&a, &b.id, &[&b.addr.to_string()]);
    let b_home = b.home().to_path_buf();
    let answerer = std::thread::spawn(move || {
        let spool = Spool::new(&b_home).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let (qid, rec) = loop {
            if let Some(x) = spool.list(Dir::Inbox, |_| true).unwrap().into_iter().next() {
                break x;
            }
            assert!(Instant::now() < deadline, "no question reached B");
            std::thread::sleep(Duration::from_millis(20));
        };
        let q = payload(&rec);
        // Forged first (lower id sorts first: UUIDv7 is time-ordered), genuine 1 s later.
        let forged = Payload::answer(&q, "not from Bea", "fake", 0, false);
        spool
            .put(
                Dir::Outbox,
                &forged.id,
                &record(&Envelope::sign(&forged, &id(99)), "unacked"),
            )
            .unwrap();
        std::thread::sleep(Duration::from_secs(1));
        let ans = Payload::answer(&q, ANSWER, "fake", 0, false);
        spool
            .put(
                Dir::Outbox,
                &ans.id,
                &record(
                    &Envelope::sign(&ans, &Identity::from_seed([2; 32])),
                    "unacked",
                ),
            )
            .unwrap();
        (qid, forged.id, ans.id)
    });
    let out = ask(home.path(), &["--wait", "10"]);
    let (qid, forged_id, aid) = answerer.join().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), ANSWER);

    let spool = Spool::new(home.path()).unwrap();
    assert!(spool.get(Dir::Inbox, &forged_id).unwrap().is_none());
    assert!(spool.get(Dir::Inbox, &aid).unwrap().is_some());
    assert_eq!(ids(&spool, Dir::Inbox), std::slice::from_ref(&aid));
    assert_eq!(
        spool.get(Dir::Done, &qid).unwrap().unwrap().state,
        "answered"
    );
    let bs = b.spool();
    assert_eq!(
        bs.get(Dir::Outbox, &forged_id).unwrap().unwrap().state,
        "unacked",
        "forged entry never acked"
    );
    assert!(bs.get(Dir::Done, &forged_id).unwrap().is_none());
    assert_eq!(bs.get(Dir::Done, &aid).unwrap().unwrap().state, "acked");
    b.running.shutdown();
}

/// Runs `owl ask --wait 10` against a scripted fake peer and returns (output, peer, home).
async fn ask_wait_fake(a: &Identity, script: FakeScript) -> (Output, FakePeer, TempDir) {
    let peer = spawn_fake(2, a, script).await;
    let home = asker_home(a, &peer.state.id, &[&peer.addr.to_string()]);
    let out = ask(home.path(), &["--wait", "10"]);
    (out, peer, home)
}

fn assert_answered_via_wait(out: &Output, peer: &FakePeer, home: &Path) -> (String, String) {
    let qid = peer.questions()[0].id.clone();
    let aid = peer.answers()[0].id.clone();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(out));
    assert_eq!(stdout(out).trim(), ANSWER);
    let spool = Spool::new(home).unwrap();
    let inbox = spool
        .get(Dir::Inbox, &aid)
        .unwrap()
        .expect("answer in inbox");
    assert_eq!(inbox.state, "pending");
    assert_eq!(inbox.meta["in_reply_to"], qid);
    let cached = spool
        .cache_get(&question_hash(PROJECT, Some(PATH), QUESTION))
        .unwrap()
        .expect("cached");
    assert_eq!(cached.raw, inbox.raw);
    assert!(
        spool.get(Dir::Asks, &qid).unwrap().is_none(),
        "ask left asks/"
    );
    assert_eq!(
        spool.get(Dir::Done, &qid).unwrap().unwrap().state,
        "answered"
    );
    (qid, aid)
}

/// The peer's outbox is unreachable for the first ~3.5 s (daemon restarting, say), so the polls
/// at t=0 and t=2 s both fail: `--wait` warns exactly once and keeps polling until the deadline
/// instead of giving up on the first error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_keeps_polling_through_transient_outbox_errors() {
    let a = id(1);
    let started = Instant::now();
    let (out, peer, home) = ask_wait_fake(
        &a,
        FakeScript {
            outbox_fails_until: Some(Instant::now() + Duration::from_millis(3500)),
            ..well_formed()
        },
    )
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "answered once the outbox recovered, not at the deadline"
    );
    let (_qid, aid) = assert_answered_via_wait(&out, &peer, home.path());
    let err = stderr(&out);
    assert_eq!(
        err.matches("warning: polling Bea").count(),
        1,
        "one warning for two failing polls, not one per poll: {err}"
    );
    assert!(
        started.elapsed() >= Duration::from_millis(3500),
        "the answer cannot arrive before the outbox recovers"
    );
    assert!(err.contains("500"), "names the peer's status: {err}");
    assert_eq!(peer.acked(), [aid], "acked once the answer was stored");
    peer.shutdown();

    // --quiet: same outcome, no warning at all.
    let peer = spawn_fake(
        2,
        &a,
        FakeScript {
            outbox_fails_until: Some(Instant::now() + Duration::from_millis(3500)),
            ..well_formed()
        },
    )
    .await;
    let home = asker_home(&a, &peer.state.id, &[&peer.addr.to_string()]);
    let out = ask(home.path(), &["--wait", "10", "--quiet"]);
    assert_answered_via_wait(&out, &peer, home.path());
    assert!(stderr(&out).is_empty(), "{}", stderr(&out));
    peer.shutdown();
}

/// The answer is stored, cached and the ask filed before the ack; a refused ack (anything but
/// `204`) is surfaced as a warning naming the answer, and the exchange still succeeds — the
/// unacked outbox entry is the responder's to expire (§3.5).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_warns_when_the_ack_is_refused() {
    let a = id(1);
    for status in [500u16, 404, 200] {
        let (out, peer, home) = ask_wait_fake(
            &a,
            FakeScript {
                ack_status: status,
                ..well_formed()
            },
        )
        .await;
        let (_qid, aid) = assert_answered_via_wait(&out, &peer, home.path());
        let err = stderr(&out);
        assert!(
            err.contains(&format!("warning: ack of {aid} failed")),
            "{status}: {err}"
        );
        assert!(err.contains("ack refused by script"), "{status}: {err}");
        assert!(
            peer.acked().is_empty(),
            "{status}: the fake never recorded an ack"
        );
        peer.shutdown();
    }
    // The 204 twin: identical flow, acked, silent.
    let (out, peer, home) = ask_wait_fake(&a, well_formed()).await;
    let (_qid, aid) = assert_answered_via_wait(&out, &peer, home.path());
    assert!(!stderr(&out).contains("warning"), "{}", stderr(&out));
    assert_eq!(peer.acked(), [aid]);
    peer.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_times_out_with_exit_4_and_keeps_the_ask_waiting() {
    let a = id(1);
    let b = spawn_daemon(2, true, &[Peer::new(&a, "Ana", manual())]).await;
    let home = asker_home(&a, &b.id, &[&b.addr.to_string()]);
    let out = ask(home.path(), &["--wait", "0"]);
    assert_eq!(out.status.code(), Some(4), "{}", stderr(&out));
    assert!(stderr(&out).contains("timeout"), "{}", stderr(&out));
    assert!(stdout(&out).is_empty());
    let spool = Spool::new(home.path()).unwrap();
    let asks = ids(&spool, Dir::Asks);
    assert_eq!(asks.len(), 1);
    let rec = spool.get(Dir::Asks, &asks[0]).unwrap().unwrap();
    assert_eq!(rec.state, "waiting");
    assert!(stderr(&out).contains(&asks[0]), "names the ask id");
    assert!(ids(&spool, Dir::Inbox).is_empty());
    assert!(ids(&spool, Dir::Cache).is_empty());
    assert!(ids(&spool, Dir::Done).is_empty());
    assert!(b.spool().get(Dir::Inbox, &asks[0]).unwrap().is_some());

    // An answer to a DIFFERENT question in B's outbox is not ours: still a timeout.
    let bs = b.spool();
    let other_q = Payload::question(&fp(&a), &fp(&b.id), PROJECT, Some(PATH), "other?");
    let other = Envelope::sign(
        &Payload::answer(&other_q, "not yours", "fake", 0, false),
        &b.id,
    );
    let other_id = payload(&record(&other, "unacked")).id;
    bs.put(Dir::Outbox, &other_id, &record(&other, "unacked"))
        .unwrap();
    let out = ask_with(
        home.path(),
        home.path(),
        "Second question?",
        &["--wait", "1"],
    );
    assert_eq!(out.status.code(), Some(4), "{}", stderr(&out));
    assert_eq!(ids(&spool, Dir::Asks).len(), 2);
    assert!(ids(&spool, Dir::Inbox).is_empty());
    assert!(
        bs.get(Dir::Outbox, &other_id).unwrap().is_some(),
        "not acked"
    );
    b.running.shutdown();
}

// AC6
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unavailable_and_rate_limited_exit_codes() {
    let a = id(1);
    // 403: policy never.
    let never = spawn_daemon(
        2,
        true,
        &[Peer::new(&a, "Ana", Some(policy(Mode::Never, None)))],
    )
    .await;
    let home = asker_home(&a, &never.id, &[&never.addr.to_string()]);
    let out = ask(home.path(), &[]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("unavailable"), "{}", stderr(&out));
    assert!(stdout(&out).is_empty());
    assert!(ids(&Spool::new(home.path()).unwrap(), Dir::Asks).is_empty());
    never.running.shutdown();

    // 403: responder disabled.
    let disabled = spawn_daemon(2, false, &[Peer::new(&a, "Ana", manual())]).await;
    let home = asker_home(&a, &disabled.id, &[&disabled.addr.to_string()]);
    let out = ask(home.path(), &[]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("unavailable"), "{}", stderr(&out));
    disabled.running.shutdown();

    // 429: one question per hour → the second ask is rate limited.
    let limited = spawn_daemon(
        2,
        true,
        &[Peer::new(&a, "Ana", Some(policy(Mode::Manual, Some(1))))],
    )
    .await;
    let home = asker_home(&a, &limited.id, &[&limited.addr.to_string()]);
    let first = accepted_id(&ask(home.path(), &[]));
    let out = ask_with(home.path(), home.path(), "Second question?", &[]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("rate limited, retry after "), "{err}");
    let secs: u64 = err
        .split("retry after ")
        .nth(1)
        .and_then(|s| s.trim_end().strip_suffix('s'))
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("retry-after seconds in {err:?}"));
    assert!((1..=3600).contains(&secs), "{secs}");
    assert_eq!(
        ids(&Spool::new(home.path()).unwrap(), Dir::Asks),
        [first],
        "only the accepted ask is recorded"
    );
    limited.running.shutdown();
}

/// `429` with and without `Retry-After`: the daemon always sends the header, so the bare arm
/// (exit 3, plain `rate limited`) and an unparseable header come from the scripted fake. Each
/// row differs from the next in the header only; nothing is spooled for any of them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rate_limited_429_with_and_without_retry_after() {
    let a = id(1);
    let body = json!({ "error": "rate limited" }).to_string();
    for (what, headers, expect_msg) in [
        ("no Retry-After", vec![], "rate limited"),
        (
            "Retry-After: 42",
            vec![("retry-after".to_string(), "42".to_string())],
            "rate limited, retry after 42s",
        ),
        (
            "Retry-After: 0",
            vec![("retry-after".to_string(), "0".to_string())],
            "rate limited, retry after 0s",
        ),
        (
            "Retry-After as an HTTP date (not seconds)",
            vec![(
                "retry-after".to_string(),
                "Wed, 21 Oct 2026 07:28:00 GMT".to_string(),
            )],
            "rate limited",
        ),
    ] {
        let peer = spawn_fake(
            2,
            &a,
            FakeScript {
                accept: Some((429, body.clone())),
                accept_headers: headers,
                ..well_formed()
            },
        )
        .await;
        let home = asker_home(&a, &peer.state.id, &[&peer.addr.to_string()]);
        let out = ask(home.path(), &[]);
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(3), "{what}: {err}");
        assert_eq!(err.trim(), format!("owl: {expect_msg}"), "{what}");
        assert!(stdout(&out).is_empty(), "{what}");
        assert!(!err.contains("panicked"), "{what}: {err}");
        assert_eq!(peer.questions().len(), 1, "{what}: the question was sent");
        let spool = Spool::new(home.path()).unwrap();
        for dir in Dir::ALL {
            assert!(
                ids(&spool, dir).is_empty(),
                "{what}: {} must stay empty",
                dir.name()
            );
        }
        peer.shutdown();
    }
}

/// A `200` is an answer only with its `X-Owl-Signature` header: the same signed body without
/// the header is a clean exit 1 naming the header, and nothing is spooled; with it, the answer
/// is verified, printed, stored and cached like `answer_200_is_verified_stored_and_cached`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn answer_200_without_signature_header_is_a_clean_error() {
    let a = id(1);
    let b = id(2);
    let hash = question_hash(PROJECT, Some(PATH), QUESTION);
    // The responder cache is shared across peers (§7): the 200 body may answer C's question.
    let c = id(3);
    let earlier = Payload::question(&fp(&c), &fp(&b), PROJECT, Some(PATH), QUESTION);
    let ans = Payload::answer(&earlier, "From the responder cache.", "fake", 1, false);
    let env = Envelope::sign(&ans, &b);

    let peer = spawn_fake(
        2,
        &a,
        FakeScript {
            accept: Some((200, env.raw.clone())),
            accept_headers: vec![],
            ..well_formed()
        },
    )
    .await;
    let home = asker_home(&a, &peer.state.id, &[&peer.addr.to_string()]);
    let out = ask(home.path(), &[]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(
        err.contains("200 answer without X-Owl-Signature header"),
        "{err}"
    );
    assert!(!err.contains("panicked"), "{err}");
    assert!(stdout(&out).is_empty());
    assert_eq!(peer.questions().len(), 1, "the question was sent");
    let spool = Spool::new(home.path()).unwrap();
    for dir in Dir::ALL {
        assert!(
            ids(&spool, dir).is_empty(),
            "{} must stay empty",
            dir.name()
        );
    }
    peer.shutdown();

    // Twin: the same body with its signature header is the verified answer.
    let peer = spawn_fake(
        2,
        &a,
        FakeScript {
            accept: Some((200, env.raw.clone())),
            accept_headers: vec![("x-owl-signature".to_string(), env.sig.clone())],
            ..well_formed()
        },
    )
    .await;
    let home = asker_home(&a, &peer.state.id, &[&peer.addr.to_string()]);
    let out = ask(home.path(), &[]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "From the responder cache.");
    let spool = Spool::new(home.path()).unwrap();
    let inbox = spool
        .get(Dir::Inbox, &ans.id)
        .unwrap()
        .expect("answer in inbox");
    assert_eq!(inbox.raw, env.raw);
    assert_eq!(inbox.sig, env.sig);
    assert_eq!(spool.cache_get(&hash).unwrap().unwrap().raw, env.raw);
    assert!(ids(&spool, Dir::Asks).is_empty(), "answered: nothing waits");
    assert_eq!(ids(&spool, Dir::Done).len(), 1, "the question is filed");
    peer.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn answer_200_is_verified_stored_and_cached() {
    let a = id(1);
    let b = spawn_daemon(2, true, &[Peer::new(&a, "Ana", manual())]).await;
    let hash = question_hash(PROJECT, Some(PATH), QUESTION);
    // B's responder cache holds the answer to an earlier, equivalent question from C.
    let c = id(3);
    let earlier = Payload::question(&fp(&c), &fp(&b.id), PROJECT, Some(PATH), QUESTION);
    let ans = Payload::answer(&earlier, "From the responder cache.", "fake", 1, false);
    let env = Envelope::sign(&ans, &b.id);
    b.spool()
        .cache_put(&hash, &record(&env, "pending"))
        .unwrap();

    let home = asker_home(&a, &b.id, &[&b.addr.to_string()]);
    let out = ask(home.path(), &[]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "From the responder cache.");
    let spool = Spool::new(home.path()).unwrap();
    assert!(ids(&spool, Dir::Asks).is_empty(), "answered: nothing waits");
    let inbox = spool
        .get(Dir::Inbox, &ans.id)
        .unwrap()
        .expect("answer in inbox");
    assert_eq!(inbox.state, "pending");
    assert_eq!(inbox.raw, env.raw);
    assert_eq!(inbox.sig, env.sig);
    assert_eq!(spool.cache_get(&hash).unwrap().unwrap().raw, env.raw);
    let done = ids(&spool, Dir::Done);
    assert_eq!(done.len(), 1, "the question is filed as answered");
    let done_rec = spool.get(Dir::Done, &done[0]).unwrap().unwrap();
    assert_eq!(done_rec.state, "answered");
    assert_eq!(payload(&done_rec).kind, Kind::Question);
    assert_eq!(done_rec.meta["answer"], ans.id);
    assert!(
        b.spool().get(Dir::Inbox, &done[0]).unwrap().is_none(),
        "B never spooled it"
    );

    // Same setup, but the cached answer is signed by someone else: refused, nothing stored.
    let forged = spawn_daemon(2, true, &[Peer::new(&a, "Ana", manual())]).await;
    let bad = Envelope::sign(&ans, &c);
    forged
        .spool()
        .cache_put(&hash, &record(&bad, "pending"))
        .unwrap();
    let home = asker_home(&a, &forged.id, &[&forged.addr.to_string()]);
    let out = ask(home.path(), &[]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("does not verify"), "{}", stderr(&out));
    let spool = Spool::new(home.path()).unwrap();
    for dir in Dir::ALL {
        assert!(
            ids(&spool, dir).is_empty(),
            "{} must stay empty",
            dir.name()
        );
    }
    b.running.shutdown();
    forged.running.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unexpected_status_is_an_error_naming_it() {
    let a = id(1);
    let b = spawn_daemon(2, true, &[Peer::new(&a, "Ana", manual())]).await;
    let home = asker_home(&a, &b.id, &[&b.addr.to_string()]);
    let book = ContactBook::load(home.path(), home.path()).unwrap();
    let contact = book.resolve("Bea").unwrap().clone();
    // A stale timestamp is the one bad request the client can be made to send.
    let mut q = question(&a, &b.id, QUESTION);
    q.ts = envelope::unix_to_rfc3339(envelope::now_unix() - 3600);
    let env = Envelope::sign(&q, &a);
    let ident = Identity::from_seed([1; 32]);
    let err = tokio::task::spawn_blocking(move || {
        client::send_question(
            &ident,
            &contact,
            &client::Iroh::Unavailable("none".into()),
            &env,
        )
    })
    .await
    .unwrap()
    .unwrap_err()
    .to_string();
    assert_eq!(err, "peer returned 400 Bad Request: stale ts");
    // And the happy path through the library maps to Accepted with the question id.
    let contact = book.resolve("Bea").unwrap().clone();
    let q = question(&a, &b.id, QUESTION);
    let env = Envelope::sign(&q, &a);
    let ident = Identity::from_seed([1; 32]);
    let out = tokio::task::spawn_blocking(move || {
        client::send_question(
            &ident,
            &contact,
            &client::Iroh::Unavailable("none".into()),
            &env,
        )
    })
    .await
    .unwrap()
    .unwrap();
    assert!(
        matches!(out, SendOutcome::Accepted { ref id } if *id == q.id),
        "{out:?}"
    );
    b.running.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn project_comes_from_origin_remote_unless_overridden() {
    let a = id(1);
    let b = spawn_daemon(2, true, &[Peer::new(&a, "Ana", manual())]).await;
    let home = asker_home(&a, &b.id, &[&b.addr.to_string()]);
    let repo = fixture_repo(&[("ana@example.org", 1)]);
    git(
        repo.path(),
        &["remote", "add", "origin", "git@github.com:org/repo.git"],
    );
    let project_of = |out: &Output| {
        let qid = accepted_id(out);
        match payload(&b.spool().get(Dir::Inbox, &qid).unwrap().unwrap()).body {
            Body::Question { project, .. } => project,
            Body::Answer { .. } => unreachable!(),
        }
    };
    // Detected from the remote, also from a subdirectory of the repo.
    let out = ask_no_project(home.path(), repo.path(), "one?");
    assert_eq!(project_of(&out), "github.com/org/repo");
    let sub = repo.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let out = ask_no_project(home.path(), &sub, "two?");
    assert_eq!(project_of(&out), "github.com/org/repo");
    // --project wins over detection.
    let out = owl(home.path(), repo.path())
        .args(["ask", "Bea", PATH, "three?", "--project", "custom/id"])
        .output()
        .unwrap();
    assert_eq!(project_of(&out), "custom/id");
    // No repo: the directory name.
    let plain = tempfile::tempdir().unwrap();
    let dir = plain.path().join("plainproj");
    std::fs::create_dir(&dir).unwrap();
    let out = ask_no_project(home.path(), &dir, "four?");
    assert_eq!(project_of(&out), "plainproj");
    // The cache key follows the detected project: the same question in another project
    // is a different hash (both asks were accepted, so nothing was cached either way).
    let spool = Spool::new(home.path()).unwrap();
    assert_eq!(ids(&spool, Dir::Asks).len(), 4);
    b.running.shutdown();
}

// AC7
#[test]
fn blame_candidates() {
    let (a, ana, zed) = (id(1), id(2), id(3));
    // 20 blamed lines; the unmatched author owns 11 of them, so a share over matched lines
    // only (6/9, 3/9) is nowhere near a share over all blamed lines (6/20, 3/20).
    // Zed owns more lines than Ana but sorts after her by name, Ana is committed first, and
    // Ana's fingerprint sorts first (the contact book's own order): the expected order
    // [Zed, Ana] is the line-share order and none of the name, commit or book orders.
    let repo = fixture_repo(&[
        ("ana@example.org", 3),
        ("zed@example.org", 6),
        ("nobody@example.org", 11),
    ]);
    assert!(
        fp(&ana) < fp(&zed),
        "book order is fingerprint order: Ana first"
    );
    let home = tempfile::tempdir().unwrap();
    prepare_home(
        home.path(),
        &a,
        true,
        &[Peer::new(&ana, "Ana", None), Peer::new(&zed, "Zed", None)],
    );
    let out = owl(home.path(), repo.path())
        .args(["ask", "--file", "f.txt", "Who owns this?", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
    let list = v.as_array().expect("JSON list");
    assert_eq!(list.len(), 2, "{v}");
    let names: Vec<&str> = list
        .iter()
        .map(|c| c.get("name").and_then(Value::as_str).unwrap())
        .collect();
    assert_eq!(
        names,
        ["Zed", "Ana"],
        "ordered by line share, not by name: {v}"
    );
    let lines: Vec<u64> = list
        .iter()
        .map(|c| c.get("lines").and_then(Value::as_u64).unwrap())
        .collect();
    assert_eq!(lines, [6, 3]);
    let shares: Vec<f64> = list
        .iter()
        .map(|c| c.get("share").and_then(Value::as_f64).unwrap())
        .collect();
    assert_eq!(
        shares,
        [6.0 / 20.0, 3.0 / 20.0],
        "share of ALL blamed lines"
    );
    assert_eq!(shares, [0.3, 0.15]);
    assert_eq!(
        list[0].get("fingerprint").and_then(Value::as_str),
        Some(fp(&zed).as_str())
    );
    assert_eq!(
        list[1].get("fingerprint").and_then(Value::as_str),
        Some(fp(&ana).as_str())
    );
    assert!(!stdout(&out).contains("nobody"));
    // Listing candidates sends nothing.
    assert!(ids(&Spool::new(home.path()).unwrap(), Dir::Asks).is_empty());

    // Same repo, but Zed's contact carries an email git blame never saw: only Ana is listed.
    let home2 = tempfile::tempdir().unwrap();
    prepare_home(home2.path(), &a, true, &[Peer::new(&ana, "Ana", None)]);
    write_contact_full(
        home2.path(),
        &Peer::new(&zed, "Zed", None),
        &[],
        &["zed@elsewhere.example"],
    );
    let out = owl(home2.path(), repo.path())
        .args(["ask", "--file", "f.txt", "Who owns this?", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
    let names: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.get("name").and_then(Value::as_str).unwrap())
        .collect();
    assert_eq!(names, ["Ana"], "{v}");

    // Without --json and without a terminal: the list goes to stderr with a hint, exit 1,
    // numbered in line-share order.
    let out = owl(home.path(), repo.path())
        .args(["ask", "--file", "f.txt", "Who owns this?"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    let zed_line = err
        .lines()
        .find(|l| l.starts_with("1) Zed"))
        .unwrap_or_else(|| panic!("Zed is candidate 1: {err}"));
    assert!(zed_line.contains("6 lines (30%)"), "{zed_line}");
    let ana_line = err
        .lines()
        .find(|l| l.starts_with("2) Ana"))
        .unwrap_or_else(|| panic!("Ana is candidate 2: {err}"));
    assert!(ana_line.contains("3 lines (15%)"), "{ana_line}");
    assert!(!err.contains("3)"), "exactly two candidates: {err}");
    assert!(err.contains("--peer"), "{err}");
    assert!(stdout(&out).is_empty());

    // A file git knows nothing about: blame fails cleanly.
    std::fs::write(repo.path().join("untracked.txt"), "x\n").unwrap();
    let out = owl(home.path(), repo.path())
        .args(["ask", "--file", "untracked.txt", "Who owns this?", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("git blame"), "{}", stderr(&out));

    // No contact matches any author: a clear error, exit 1.
    let home3 = tempfile::tempdir().unwrap();
    prepare_home(home3.path(), &a, true, &[Peer::new(&zed, "Yul", None)]);
    let out = owl(home3.path(), repo.path())
        .args(["ask", "--file", "f.txt", "Who owns this?"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("no contact matches"),
        "{}",
        stderr(&out)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_with_peer_skips_blame_and_sends() {
    let a = id(1);
    let b = spawn_daemon(2, true, &[Peer::new(&a, "Ana", manual())]).await;
    let home = asker_home(&a, &b.id, &[&b.addr.to_string()]);
    // Not even a git repo: with --peer, blame never runs and the file is just the path.
    let out = owl(home.path(), home.path())
        .args([
            "ask",
            "--file",
            "src/nowhere.rs",
            "--peer",
            "bea@example.org",
            "Why?",
            "--project",
            PROJECT,
        ])
        .output()
        .unwrap();
    let qid = accepted_id(&out);
    let rec = b.spool().get(Dir::Inbox, &qid).unwrap().unwrap();
    assert_eq!(
        payload(&rec).body,
        Body::Question {
            project: PROJECT.into(),
            path: Some("src/nowhere.rs".into()),
            question: "Why?".into()
        }
    );
    // Missing question with --file is a usage error, nothing sent.
    let out = owl(home.path(), home.path())
        .args(["ask", "--file", "src/nowhere.rs", "--peer", "Bea"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("missing <question>"),
        "{}",
        stderr(&out)
    );
    assert_eq!(ids(&Spool::new(home.path()).unwrap(), Dir::Asks).len(), 1);
    b.running.shutdown();
}
