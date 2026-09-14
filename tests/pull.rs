//! OWL-008: the daemon's pull loop. Two in-process daemons, A (asker) and B (responder), with
//! A's `pull_interval_secs = 1`. Answers are placed straight into B's `outbox/` (what `owl
//! send` does) and A's loop is expected to verify, ingest, ack and report them.
//!
//! AC1 `answer_is_pulled_and_acked`, AC2 `forged_answer_is_dropped`, AC3
//! `unrelated_answer_is_ignored` + `answer_for_another_peers_ask_is_ignored`, AC4
//! `offline_responder_is_skipped_then_retried`, AC5 `outbox_ttl_expires`, AC6
//! `status_file_is_written_each_loop`; `asks_are_grouped_by_responder` pins one probe per
//! responder.

mod common;

use std::io::Write;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use axum_server::Handle;
use axum_server::tls_rustls::{RustlsAcceptor, RustlsConfig};
use owlpost::contacts::Mode;
use owlpost::envelope::{self, Envelope, Kind, Payload};
use owlpost::identity::Identity;
use owlpost::pull::{self, PullStatus, STATUS_FILE, read_status};
use owlpost::spool::{Dir, Record, Spool};
use tracing_subscriber::fmt::MakeWriter;

use common::{Peer, TestDaemon, fp, id, policy, question, record, spawn_daemon_with};

const ANSWER: &str = "because the session cookie is renewed on every request";

// ---- log capture -----------------------------------------------------------------------------
// One process-wide subscriber (tests in this binary share it); each test looks for lines that
// carry its own ids, so concurrent tests do not confuse each other.

#[derive(Clone, Default)]
struct LogSink(Arc<Mutex<Vec<u8>>>);

impl Write for LogSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for LogSink {
    type Writer = LogSink;
    fn make_writer(&'a self) -> LogSink {
        self.clone()
    }
}

fn logs() -> &'static LogSink {
    static SINK: OnceLock<LogSink> = OnceLock::new();
    SINK.get_or_init(|| {
        let sink = LogSink::default();
        tracing_subscriber::fmt()
            .with_writer(sink.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .init();
        sink
    })
}

fn log_text() -> String {
    String::from_utf8_lossy(&logs().0.lock().unwrap()).into_owned()
}

/// Bounded wait for `pred` (checked every 25 ms); panics with `what` on timeout.
fn wait_until(limit: Duration, what: &str, mut pred: impl FnMut() -> bool) -> Duration {
    let start = Instant::now();
    while !pred() {
        assert!(start.elapsed() < limit, "timed out after {limit:?}: {what}");
        std::thread::sleep(Duration::from_millis(25));
    }
    start.elapsed()
}

fn wait_for_log(limit: Duration, needle: &str) {
    wait_until(limit, &format!("log line containing {needle:?}"), || {
        log_text().contains(needle)
    });
}

// ---- fixtures ----------------------------------------------------------------------------------

/// B: a normal daemon that knows A (so A's client certificate passes the handshake).
async fn spawn_b(a: &Identity) -> TestDaemon {
    spawn_daemon_with(
        2,
        &[Peer::new(a, "Ana", Some(policy(Mode::Manual, None)))],
        |cfg| cfg.responder.enabled = true,
    )
    .await
}

/// A: pulls every second. B's contact is written separately (see `point_at`).
async fn spawn_a() -> TestDaemon {
    spawn_daemon_with(1, &[], |cfg| cfg.pull_interval_secs = 1).await
}

/// (Re)writes A's contact for B with `endpoint`; the loop reloads the book every pull.
fn point_at(a: &TestDaemon, b: &Identity, endpoint: &str) {
    common::write_contact_full(a.home(), &Peer::new(b, "Bea", None), &[endpoint], &[]);
}

/// A signed question A→B filed in A's `asks/` as `owl ask` leaves it on `202`.
fn open_ask(a: &TestDaemon, b: &Identity, text: &str) -> Payload {
    let q = question(&a.id, b, text);
    let env = Envelope::sign(&q, &a.id);
    let hash = envelope::question_hash(common::PROJECT, Some(common::PATH), text);
    let mut rec = record(&env, "waiting");
    rec.meta = serde_json::json!({ "peer": fp(b), "hash": hash });
    a.spool().put(Dir::Asks, &q.id, &rec).unwrap();
    q
}

/// `signer`'s answer to `q`, in `b`'s outbox (state `unacked`) dated `received_at`.
fn outbox_answer(
    b: &TestDaemon,
    signer: &Identity,
    q: &Payload,
    text: &str,
    received_at: Option<&str>,
) -> Payload {
    let ans = Payload::answer(q, text, "fake", 0, false);
    let env = Envelope::sign(&ans, signer);
    let mut rec = record(&env, "unacked");
    if let Some(at) = received_at {
        rec.received_at = at.into();
    }
    b.spool().put(Dir::Outbox, &ans.id, &rec).unwrap();
    ans
}

fn payload(rec: &Record) -> Payload {
    serde_json::from_str(&rec.raw).unwrap()
}

fn closed_port() -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().to_string()
}

fn status(home: &Path) -> Option<PullStatus> {
    read_status(home).unwrap()
}

/// Everything AC1 promises about an ingested answer, on both sides.
fn assert_ingested(a: &TestDaemon, b: &TestDaemon, q: &Payload, ans: &Payload, text: &str) {
    let spool = a.spool();
    let inbox = spool
        .get(Dir::Inbox, &ans.id)
        .unwrap()
        .expect("answer in A's inbox");
    assert_eq!(inbox.state, "pending");
    assert!(!inbox.seen);
    let p = payload(&inbox);
    assert_eq!(p.kind, Kind::Answer);
    assert_eq!(p.in_reply_to.as_deref(), Some(q.id.as_str()));
    assert_eq!(inbox.meta["peer"], b.fp());
    assert_eq!(inbox.meta["in_reply_to"], q.id);
    let hash = envelope::question_hash(common::PROJECT, Some(common::PATH), text);
    assert_eq!(inbox.meta["hash"], hash);
    let cached = spool.cache_get(&hash).unwrap().expect("answer cached");
    assert_eq!(cached.raw, inbox.raw);
    assert_eq!(cached.sig, inbox.sig);
    assert!(
        spool.list(Dir::Asks, |_| true).unwrap().is_empty(),
        "asks/ is empty"
    );
    assert_eq!(
        spool.get(Dir::Done, &q.id).unwrap().unwrap().state,
        "answered"
    );
    let bs = b.spool();
    assert!(
        bs.get(Dir::Outbox, &ans.id).unwrap().is_none(),
        "acked answer left B's outbox"
    );
    assert_eq!(bs.get(Dir::Done, &ans.id).unwrap().unwrap().state, "acked");
}

// ---- tests ---------------------------------------------------------------------------------------

// AC1
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn answer_is_pulled_and_acked() {
    logs();
    let a = spawn_a().await;
    let b = spawn_b(&a.id).await;
    point_at(&a, &b.id, &b.addr.to_string());
    let q = open_ask(&a, &b.id, "why does session expiry drift?");
    let ans = outbox_answer(&b, &b.id, &q, ANSWER, None);

    let took = wait_until(Duration::from_secs(5), "answer acked on B", || {
        b.spool().get(Dir::Done, &ans.id).unwrap().is_some()
    });
    // The ack is the last step of the ingestion, so everything else is already in place.
    assert_ingested(&a, &b, &q, &ans, "why does session expiry drift?");
    assert!(took < Duration::from_secs(5), "{took:?}");
    // The daemon's event path fired (this is what `notify` hangs off).
    wait_for_log(Duration::from_secs(2), &format!("id={} ", q.id));
    let log = log_text();
    let line = log
        .lines()
        .find(|l| l.contains("answer ingested") && l.contains(&q.id))
        .unwrap_or_else(|| panic!("no 'answer ingested' line for {}:\n{log}", q.id));
    assert!(line.contains(&b.fp()), "{line}");
    assert!(line.contains(common::PATH), "{line}");
    // Status after the loop: nothing open, one peer probed on the last pull.
    wait_until(Duration::from_secs(3), "status with 0 open asks", || {
        status(a.home()).is_some_and(|s| s.open_asks == 0)
    });
    a.running.shutdown();
    b.running.shutdown();
}

// AC2
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forged_answer_is_dropped() {
    logs();
    let a = spawn_a().await;
    let b = spawn_b(&a.id).await;
    point_at(&a, &b.id, &b.addr.to_string());
    let q = open_ask(&a, &b.id, "who signs this?");
    let forged = outbox_answer(&b, &id(99), &q, "not really from Bea", None);

    // The warning names the entry; wait for the loop to have seen it (twice, to be sure the
    // first verdict was not a one-off).
    wait_for_log(
        Duration::from_secs(5),
        &format!("claimed_id={} ", forged.id),
    );
    let first = log_text().matches("signature does not verify").count();
    wait_until(Duration::from_secs(5), "a second pull", || {
        log_text().matches("signature does not verify").count() > first
    });
    let log = log_text();
    let line = log
        .lines()
        .find(|l| l.contains("signature does not verify") && l.contains(&forged.id))
        .unwrap();
    assert!(line.contains("WARN"), "logged as a warning: {line}");
    assert!(line.contains(&b.fp()), "{line}");

    let spool = a.spool();
    assert!(spool.get(Dir::Inbox, &forged.id).unwrap().is_none());
    assert!(spool.list(Dir::Inbox, |_| true).unwrap().is_empty());
    assert!(spool.list(Dir::Cache, |_| true).unwrap().is_empty());
    assert_eq!(
        spool.get(Dir::Asks, &q.id).unwrap().unwrap().state,
        "waiting",
        "the ask stays open"
    );
    assert!(spool.get(Dir::Done, &q.id).unwrap().is_none());
    // Not acked: still in B's outbox.
    let bs = b.spool();
    assert_eq!(
        bs.get(Dir::Outbox, &forged.id).unwrap().unwrap().state,
        "unacked"
    );
    assert!(bs.get(Dir::Done, &forged.id).unwrap().is_none());
    assert_eq!(status(a.home()).unwrap().open_asks, 1);
    a.running.shutdown();
    b.running.shutdown();
}

// AC3
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unrelated_answer_is_ignored() {
    logs();
    let a = spawn_a().await;
    let b = spawn_b(&a.id).await;
    point_at(&a, &b.id, &b.addr.to_string());
    let q = open_ask(&a, &b.id, "the real question");
    // A properly signed answer from B to a question A never filed.
    let other = question(&a.id, &b.id, "a question nobody asked");
    let stray = outbox_answer(&b, &b.id, &other, "an answer to nothing", None);

    wait_for_log(Duration::from_secs(5), &format!("id={} ", stray.id));
    let first = log_text().matches("replies to no open ask").count();
    wait_until(Duration::from_secs(5), "a second pull", || {
        log_text().matches("replies to no open ask").count() > first
    });
    let log = log_text();
    let line = log
        .lines()
        .find(|l| l.contains("replies to no open ask") && l.contains(&stray.id))
        .unwrap();
    assert!(
        line.contains(&other.id),
        "names the unknown question: {line}"
    );

    let spool = a.spool();
    assert!(spool.get(Dir::Inbox, &stray.id).unwrap().is_none());
    assert!(spool.list(Dir::Inbox, |_| true).unwrap().is_empty());
    assert!(spool.list(Dir::Cache, |_| true).unwrap().is_empty());
    assert_eq!(
        spool.get(Dir::Asks, &q.id).unwrap().unwrap().state,
        "waiting"
    );
    let bs = b.spool();
    assert_eq!(
        bs.get(Dir::Outbox, &stray.id).unwrap().unwrap().state,
        "unacked",
        "not acked"
    );
    assert!(bs.get(Dir::Done, &stray.id).unwrap().is_none());
    assert!(
        !log.lines()
            .any(|l| l.contains("signature does not verify") && l.contains(&stray.id)),
        "a genuine signature is not reported as forged"
    );
    a.running.shutdown();
    b.running.shutdown();
}

// AC4
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn offline_responder_is_skipped_then_retried() {
    logs();
    let a = spawn_a().await;
    // B's home is prepared on a fixed, currently closed port; B itself is not started yet.
    // Seed 22, not 2: the log filters below key on B's fingerprint, which must not collide
    // with the B of the other tests in this binary.
    let b_id = id(22);
    let b_dir = tempfile::tempdir().unwrap();
    let port = closed_port();
    common::prepare_home_with(
        b_dir.path(),
        &b_id,
        &[Peer::new(&a.id, "Ana", Some(policy(Mode::Manual, None)))],
        |cfg| {
            cfg.responder.enabled = true;
            cfg.listen = port.clone();
        },
    );
    point_at(&a, &b_id, &port);
    let q = open_ask(&a, &b_id, "are you there?");
    let ans = Payload::answer(&q, ANSWER, "fake", 0, false);
    Spool::new(b_dir.path())
        .unwrap()
        .put(
            Dir::Outbox,
            &ans.id,
            &record(&Envelope::sign(&ans, &b_id), "unacked"),
        )
        .unwrap();
    let started = Instant::now();

    let failed_probes = || {
        log_text()
            .lines()
            .filter(|l| l.contains("outbox fetch failed") && l.contains(&port))
            .count()
    };
    let b_fp = fp(&b_id);
    let fetched = || {
        log_text()
            .lines()
            .filter(|l| l.contains("outbox fetched") && l.contains(&b_fp))
            .count()
    };
    // While B is down the same endpoint is probed again and again (≥ 2 failures, so the
    // liveness cache did not park it), and nothing is ingested.
    wait_until(Duration::from_secs(5), "two failed probes", || {
        failed_probes() >= 2
    });
    assert_eq!(fetched(), 0);
    assert!(
        a.spool().get(Dir::Inbox, &ans.id).unwrap().is_none(),
        "nothing ingested while offline"
    );
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "at least two loops apart"
    );
    let failures_before = failed_probes();

    // Start B on that very port; A's next probe succeeds and the answer is ingested.
    let b = common::respawn(b_dir, b_id).await;
    assert_eq!(b.addr.to_string(), port);
    wait_until(
        Duration::from_secs(7),
        "answer ingested after B came up",
        || b.spool().get(Dir::Done, &ans.id).unwrap().is_some(),
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert!(fetched() >= 1, "a successful probe was logged");
    assert!(failures_before >= 2);
    assert_ingested(&a, &b, &q, &ans, "are you there?");
    a.running.shutdown();
    b.running.shutdown();
}

/// AC3, two contacts: a B-signed answer to A's open ask to C is verified but not B's to
/// answer — nothing is ingested, cached or acked, and the ask to C stays open.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn answer_for_another_peers_ask_is_ignored() {
    logs();
    let a = spawn_a().await;
    let b = spawn_b(&a.id).await;
    let c = id(3);
    point_at(&a, &b.id, &b.addr.to_string());
    common::write_contact_full(a.home(), &Peer::new(&c, "Cy", None), &[&closed_port()], &[]);
    let to_b = open_ask(&a, &b.id, "for Bea");
    let to_c = open_ask(&a, &c, "for Cy");
    // B answers Cy's question, and its own.
    let hijack = outbox_answer(&b, &b.id, &to_c, "Bea answering for Cy", None);
    let genuine = outbox_answer(&b, &b.id, &to_b, ANSWER, None);

    wait_until(Duration::from_secs(5), "genuine answer acked", || {
        b.spool().get(Dir::Done, &genuine.id).unwrap().is_some()
    });
    wait_for_log(Duration::from_secs(5), &format!("id={} ", hijack.id));
    let spool = a.spool();
    assert!(spool.get(Dir::Inbox, &hijack.id).unwrap().is_none());
    assert_eq!(
        spool.list(Dir::Inbox, |_| true).unwrap().len(),
        1,
        "only the genuine answer"
    );
    assert_eq!(
        spool.get(Dir::Asks, &to_c.id).unwrap().unwrap().state,
        "waiting",
        "the ask to C stays open"
    );
    assert!(spool.get(Dir::Done, &to_c.id).unwrap().is_none());
    let c_hash = envelope::question_hash(common::PROJECT, Some(common::PATH), "for Cy");
    assert!(
        spool.cache_get(&c_hash).unwrap().is_none(),
        "nothing cached for C's question"
    );
    let bs = b.spool();
    assert_eq!(
        bs.get(Dir::Outbox, &hijack.id).unwrap().unwrap().state,
        "unacked",
        "not acked"
    );
    assert!(bs.get(Dir::Done, &hijack.id).unwrap().is_none());
    let log = log_text();
    assert!(
        log.lines()
            .any(|l| l.contains("no open ask to this peer") && l.contains(&hijack.id)),
        "{log}"
    );
    a.running.shutdown();
    b.running.shutdown();
}

/// Two open asks to one responder are served by a single probe of that responder.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn asks_are_grouped_by_responder() {
    logs();
    let a_id = id(1);
    let b = spawn_b(&a_id).await;
    let a_dir = tempfile::tempdir().unwrap();
    common::prepare_home_with(a_dir.path(), &a_id, &[], |_| {});
    common::write_contact_full(
        a_dir.path(),
        &Peer::new(&b.id, "Bea", None),
        &[&b.addr.to_string()],
        &[],
    );
    let spool = Spool::new(a_dir.path()).unwrap();
    let mut asks = Vec::new();
    for text in ["first", "second"] {
        let q = question(&a_id, &b.id, text);
        let mut rec = record(&Envelope::sign(&q, &a_id), "waiting");
        rec.meta = serde_json::json!({ "peer": b.fp(), "hash": text });
        spool.put(Dir::Asks, &q.id, &rec).unwrap();
        asks.push(outbox_answer(&b, &b.id, &q, text, None));
    }
    // `pull_once` uses the blocking client, so it runs off the async runtime as the loop does.
    let home = a_dir.path().to_path_buf();
    let (status, events) = tokio::task::spawn_blocking(move || {
        let spool = Spool::new(&home).unwrap();
        let mut events = Vec::new();
        let mut liveness = pull::Liveness::new(Duration::ZERO);
        let status = pull::pull_once(
            &home,
            &home,
            &a_id,
            &owlpost::client::Iroh::Unavailable("none".into()),
            &spool,
            &mut liveness,
            Instant::now(),
            |ev| events.push(ev),
        )
        .unwrap();
        (status, events)
    })
    .await
    .unwrap();
    assert_eq!(status.peers_probed, 1, "one probe for both asks");
    assert_eq!(status.open_asks, 0);
    assert_eq!(events.len(), 2);
    assert!(
        events
            .iter()
            .all(|e| matches!(e, owlpost::server::DaemonEvent::Answer(a) if a.peer == b.fp()))
    );
    assert!(spool.list(Dir::Asks, |_| true).unwrap().is_empty());
    for (ans, text) in asks.iter().zip(["first", "second"]) {
        assert_eq!(
            spool.get(Dir::Inbox, &ans.id).unwrap().unwrap().state,
            "pending"
        );
        assert!(spool.cache_get(text).unwrap().is_some());
        assert_eq!(
            b.spool().get(Dir::Done, &ans.id).unwrap().unwrap().state,
            "acked"
        );
    }
    b.running.shutdown();
}

// AC5
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outbox_ttl_expires() {
    logs();
    let a = id(1);
    let dir = tempfile::tempdir().unwrap();
    let b = id(2);
    common::prepare_home_with(dir.path(), &b, &[Peer::new(&a, "Ana", None)], |cfg| {
        cfg.outbox_ttl_days = 0;
        // Default 60 s interval: only the first loop can run inside this test.
        assert_eq!(cfg.pull_interval_secs, 60);
    });
    let spool = Spool::new(dir.path()).unwrap();
    let q = question(&a, &b, "an old question");
    let now = envelope::now_unix();
    let yesterday = envelope::unix_to_rfc3339(now - 86_400);
    let old = Payload::answer(&q, "stale", "fake", 0, false);
    let mut rec = record(&Envelope::sign(&old, &b), "unacked");
    rec.received_at = yesterday;
    spool.put(Dir::Outbox, &old.id, &rec).unwrap();
    // A record inside the TTL must stay. It is dated a minute ahead rather than "this
    // second": with a 0-day TTL the first tick lands a few hundred ms after `now` (the iroh
    // endpoint binds first), so a record stamped `now` expires whenever that crosses a
    // second boundary. The exact `<=` boundary is pinned by `pull::tests::expire_outbox_boundary`.
    let fresh = Payload::answer(&q, "fresh", "fake", 0, false);
    let mut rec = record(&Envelope::sign(&fresh, &b), "unacked");
    rec.received_at = envelope::unix_to_rfc3339(now + 60);
    spool.put(Dir::Outbox, &fresh.id, &rec).unwrap();

    let d = common::respawn(dir, b).await;
    wait_until(Duration::from_secs(3), "expired on the first loop", || {
        d.spool().get(Dir::Done, &old.id).unwrap().is_some()
    });
    let bs = d.spool();
    assert_eq!(
        bs.get(Dir::Done, &old.id).unwrap().unwrap().state,
        "expired"
    );
    assert!(bs.get(Dir::Outbox, &old.id).unwrap().is_none());
    assert_eq!(
        bs.get(Dir::Outbox, &fresh.id).unwrap().unwrap().state,
        "unacked",
        "not older than the TTL: kept"
    );
    assert!(bs.get(Dir::Done, &fresh.id).unwrap().is_none());
    d.running.shutdown();
}

// AC6
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_file_is_written_each_loop() {
    logs();
    let a = spawn_a().await;
    assert!(a.running.pull_running());
    let path = a.home().join(STATUS_FILE);
    wait_until(Duration::from_secs(3), "first status", || path.exists());
    let raw: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let at = raw
        .get("last_pull_at")
        .and_then(|v| v.as_str())
        .expect("last_pull_at");
    let at_unix = envelope::parse_rfc3339_to_unix(at).expect("RFC 3339 UTC");
    assert!(envelope::now_unix().abs_diff(at_unix) <= 5, "{at}");
    assert_eq!(raw.get("open_asks").and_then(|v| v.as_u64()), Some(0));
    assert_eq!(raw.get("peers_probed").and_then(|v| v.as_u64()), Some(0));
    assert!(!a.home().join("daemon.status.tmp").exists());
    let first = status(a.home()).unwrap();

    // An open ask to an offline peer: the next loops report it open and the peer probed.
    let b = id(2);
    point_at(&a, &b, &closed_port());
    open_ask(&a, &b, "anyone?");
    wait_until(
        Duration::from_secs(4),
        "status reflecting the open ask",
        || {
            status(a.home()).is_some_and(|s| {
                s == PullStatus {
                    last_pull_at: s.last_pull_at.clone(),
                    open_asks: 1,
                    peers_probed: 1,
                }
            })
        },
    );
    // Rewritten every loop: the timestamp moves on.
    wait_until(Duration::from_secs(4), "a later last_pull_at", || {
        status(a.home()).is_some_and(|s| s.last_pull_at > first.last_pull_at)
    });
    a.running.shutdown();
    wait_until(Duration::from_secs(3), "pull loop stopped", || {
        !a.running.pull_running()
    });
    let TestDaemon { running, .. } = a;
    running.wait().await.unwrap();
}

// OWL-033 AC8
/// A pulled answer is a new inbox record: within 2 s of its spool write a wake file sits
/// under the live session whose cwd is the configured checkout of the question's project
/// (an answer is routed by its question's project), not under the other live session with
/// the newer heartbeat; the routing names it and the ingestion is otherwise unchanged.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pulled_answer_wakes_the_affine_live_session() {
    use owlpost::route::{self, Marker};
    logs();
    let checkout = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let checkout_path = checkout.path().to_string_lossy().into_owned();
    let a = spawn_daemon_with(1, &[], |cfg| {
        cfg.pull_interval_secs = 1;
        cfg.projects.insert(common::PROJECT.into(), checkout_path);
    })
    .await;
    let b = spawn_b(&a.id).await;
    point_at(&a, &b.id, &b.addr.to_string());
    let marker = |sid: &str, cwd: &Path, age: u64| {
        let mut m = Marker::new(sid, &cwd.to_string_lossy(), "startup");
        m.heartbeat_at = envelope::unix_to_rfc3339(envelope::now_unix() - age);
        route::write_marker(a.home(), &m).unwrap();
    };
    marker("S1", checkout.path(), 300);
    marker("S2", elsewhere.path(), 0);
    let q = open_ask(&a, &b.id, "why does session expiry drift?");
    let ans = outbox_answer(&b, &b.id, &q, ANSWER, None);
    wait_until(Duration::from_secs(5), "answer in A's inbox", || {
        a.spool().get(Dir::Inbox, &ans.id).unwrap().is_some()
    });
    let wake = route::wake_file(a.home(), "S1", &ans.id);
    let took = wait_until(Duration::from_secs(2), "wake file under S1", || {
        wake.is_file()
    });
    assert!(took < Duration::from_secs(2), "{took:?}");
    let block = std::fs::read_to_string(&wake).unwrap();
    assert_eq!(
        block.lines().next(),
        Some(route::WAKE_INSTRUCTION),
        "{block}"
    );
    assert!(block.contains(&format!("| {ANSWER} |")), "{block}");
    assert!(block.contains(ANSWER), "{block}");
    assert!(block.contains(common::PATH), "the question's path: {block}");
    assert!(
        !route::wake_file(a.home(), "S2", &ans.id).exists(),
        "S2 stays silent"
    );
    let r = route::load_routing(a.home(), &ans.id);
    assert_eq!(r.current.as_deref(), Some("S1"));
    assert_eq!(r.tried, vec!["S1".to_string()]);
    wait_until(Duration::from_secs(5), "answer acked on B", || {
        b.spool().get(Dir::Done, &ans.id).unwrap().is_some()
    });
    assert_ingested(&a, &b, &q, &ans, "why does session expiry drift?");
    a.running.shutdown();
    b.running.shutdown();
}

// ---- OWL-034: the peer's Task for every still-open ask -----------------------------------------
// AC4, daemon half. After ingesting a reached responder's answers the loop asks that peer where
// each still-open ask stands; only a `REJECTED` Task closes one (`done/declined` +
// `DaemonEvent::Declined`).

/// An asker home with no daemon of its own: iroh is unavailable (no `daemon.addr`), so every
/// request goes to `endpoint`, and no background loop competes with the `pull_once` under test.
fn asker_home(a: &Identity, b: &Identity, endpoint: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    common::prepare_home_with(dir.path(), a, &[], |_| {});
    common::write_contact_full(dir.path(), &Peer::new(b, "Bea", None), &[endpoint], &[]);
    dir
}

/// A signed question A→B filed in `home`'s `asks/` as `owl ask` leaves it on `202`.
fn file_ask(home: &Path, a: &Identity, b: &Identity, text: &str) -> Payload {
    let q = question(a, b, text);
    let env = Envelope::sign(&q, a);
    let hash = envelope::question_hash(common::PROJECT, Some(common::PATH), text);
    let mut rec = record(&env, "waiting");
    rec.meta = serde_json::json!({ "peer": fp(b), "hash": hash });
    Spool::new(home)
        .unwrap()
        .put(Dir::Asks, &q.id, &rec)
        .unwrap();
    q
}

/// Posts the very bytes of a filed ask to B over the real HTTP path, so B holds the question
/// like any arriving one (`consent`: B has no policy for A yet).
async fn post_ask(b: &TestDaemon, a: &Identity, home: &Path, q: &Payload) {
    let rec = Spool::new(home)
        .unwrap()
        .get(Dir::Asks, &q.id)
        .unwrap()
        .expect("the ask was filed");
    let env = Envelope {
        raw: rec.raw,
        sig: rec.sig,
    };
    let resp = common::post_envelope(&common::client(Some(a), &b.id), b, &env).await;
    assert_eq!(resp.status().as_u16(), 202, "B accepted the question");
    assert_eq!(
        b.spool().get(Dir::Inbox, &q.id).unwrap().unwrap().state,
        "consent",
        "B holds it for the owner's consent"
    );
}

/// One `pull_once` for the home of `seed`, run off the runtime (the client is blocking) exactly
/// as the daemon's tick runs it.
async fn pull_now(home: &Path, seed: u8) -> (PullStatus, Vec<owlpost::server::DaemonEvent>) {
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let spool = Spool::new(&home).unwrap();
        let mut events = Vec::new();
        let status = pull::pull_once(
            &home,
            &home,
            &id(seed),
            &owlpost::client::Iroh::Unavailable("no endpoint".into()),
            &spool,
            &mut pull::Liveness::new(Duration::ZERO),
            Instant::now(),
            |ev| events.push(ev),
        )
        .unwrap();
        (status, events)
    })
    .await
    .unwrap()
}

/// AC4: B holds A's question and the owner denied it — the `REJECTED` Task closes the ask as
/// `done/declined` and reports it once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rejected_task_closes_the_ask_as_declined() {
    logs();
    let a = id(31);
    let b = common::spawn_daemon(32, true, &[Peer::new(&a, "Ana", None)]).await;
    let home = asker_home(&a, &b.id, &b.addr.to_string());
    let q = file_ask(home.path(), &a, &b.id, "may I have an answer?");
    post_ask(&b, &a, home.path(), &q).await;
    // What `owl deny` does to a held question (`cli::deny::deny_held` → `answer::finish`).
    let bs = b.spool();
    let held = bs.get(Dir::Inbox, &q.id).unwrap().unwrap();
    owlpost::answer::finish(
        &bs,
        &q.id,
        held,
        "denied",
        &[("previous_state", serde_json::json!("consent"))],
        owlpost::events::Ev::by("denied", "human"),
    )
    .unwrap();

    let (status, events) = pull_now(home.path(), 31).await;
    let spool = Spool::new(home.path()).unwrap();
    assert!(
        spool.get(Dir::Asks, &q.id).unwrap().is_none(),
        "the ask left asks/"
    );
    assert_eq!(
        spool.get(Dir::Done, &q.id).unwrap().unwrap().state,
        "declined"
    );
    assert_eq!(status.open_asks, 0);
    assert_eq!(status.peers_probed, 1);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(
        events[0],
        owlpost::server::DaemonEvent::Declined(owlpost::server::Declined {
            id: q.id.clone(),
            peer: b.fp(),
        })
    );
    b.running.shutdown();
}

/// The sibling arm: the same fixture with the question still held for the owner's consent —
/// a `SUBMITTED` Task changes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn held_task_leaves_the_ask_open() {
    logs();
    let a = id(33);
    let b = common::spawn_daemon(34, true, &[Peer::new(&a, "Ana", None)]).await;
    let home = asker_home(&a, &b.id, &b.addr.to_string());
    let q = file_ask(home.path(), &a, &b.id, "may I have an answer?");
    post_ask(&b, &a, home.path(), &q).await;

    let (status, events) = pull_now(home.path(), 33).await;
    let spool = Spool::new(home.path()).unwrap();
    assert_eq!(
        spool.get(Dir::Asks, &q.id).unwrap().unwrap().state,
        "waiting",
        "still open"
    );
    assert!(spool.get(Dir::Done, &q.id).unwrap().is_none());
    assert_eq!(status.open_asks, 1);
    assert_eq!(status.peers_probed, 1);
    assert!(events.is_empty(), "{events:?}");
    // The Task really was fetched, and it was not rejected.
    let log = log_text();
    let line = log
        .lines()
        .find(|l| l.contains("task state") && l.contains(&q.id))
        .unwrap_or_else(|| panic!("no 'task state' line for {}:\n{log}", q.id));
    assert!(line.contains(envelope::TASK_STATE_SUBMITTED), "{line}");
    b.running.shutdown();
}

/// A peer that was never reached is never asked for a Task: the ask stays open, silently.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unreached_peer_is_not_asked_for_a_task() {
    logs();
    let a = id(35);
    let b = id(36);
    let home = asker_home(&a, &b, &closed_port());
    let q = file_ask(home.path(), &a, &b, "anyone home?");

    let (status, events) = pull_now(home.path(), 35).await;
    let spool = Spool::new(home.path()).unwrap();
    assert_eq!(
        spool.get(Dir::Asks, &q.id).unwrap().unwrap().state,
        "waiting"
    );
    assert!(spool.get(Dir::Done, &q.id).unwrap().is_none());
    assert_eq!(status.open_asks, 1);
    assert_eq!(status.peers_probed, 1, "probed, but not reached");
    assert!(events.is_empty(), "{events:?}");
    let log = log_text();
    assert!(
        !log.lines().any(|l| l.contains(&q.id) && l.contains("task")),
        "no task fetch for an unreachable peer:\n{log}"
    );
}

/// The `Ok(None)` arm: the peer is up but holds no record of the id (its responder is on and
/// its policy for A is not `never`, so this is a real `404`) — the ask stays open, silently.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn task_404_leaves_the_ask_open() {
    logs();
    let a = id(37);
    let b = common::spawn_daemon(
        38,
        true,
        &[Peer::new(&a, "Ana", Some(policy(Mode::Manual, None)))],
    )
    .await;
    let home = asker_home(&a, &b.id, &b.addr.to_string());
    // Filed locally and never posted: B has never seen this question.
    let q = file_ask(home.path(), &a, &b.id, "a question B never received");

    let (status, events) = pull_now(home.path(), 37).await;
    let spool = Spool::new(home.path()).unwrap();
    assert_eq!(
        spool.get(Dir::Asks, &q.id).unwrap().unwrap().state,
        "waiting"
    );
    assert!(spool.get(Dir::Done, &q.id).unwrap().is_none());
    assert_eq!(status.open_asks, 1);
    assert_eq!(status.peers_probed, 1);
    assert!(events.is_empty(), "{events:?}");
    let log = log_text();
    assert!(
        log.lines()
            .any(|l| l.contains("peer holds no task for the ask") && l.contains(&q.id)),
        "the 404 was seen:\n{log}"
    );
    b.running.shutdown();
}

/// AC4, the peer filter on the still-open ids: two responders, each holding one of A's open
/// asks, where the asks differ ONLY in whose they are (same project, same path, same words).
/// Only B's owner declined, so only B's ask is closed — and each peer is asked about its own
/// ask alone, never about the other's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_peer_is_asked_only_about_its_own_asks() {
    logs();
    const SAME: &str = "may I have an answer?";
    let a = id(39);
    let b = common::spawn_daemon(40, true, &[Peer::new(&a, "Ana", None)]).await;
    let c = common::spawn_daemon(41, true, &[Peer::new(&a, "Ana", None)]).await;
    let home = asker_home(&a, &b.id, &b.addr.to_string());
    common::write_contact_full(
        home.path(),
        &Peer::new(&c.id, "Cy", None),
        &[&c.addr.to_string()],
        &[],
    );
    let qb = file_ask(home.path(), &a, &b.id, SAME);
    let qc = file_ask(home.path(), &a, &c.id, SAME);
    post_ask(&b, &a, home.path(), &qb).await;
    post_ask(&c, &a, home.path(), &qc).await;
    // Only B's owner denied; C still holds its question for the owner's consent.
    let bs = b.spool();
    let held = bs.get(Dir::Inbox, &qb.id).unwrap().unwrap();
    owlpost::answer::finish(
        &bs,
        &qb.id,
        held,
        "denied",
        &[("previous_state", serde_json::json!("consent"))],
        owlpost::events::Ev::by("denied", "human"),
    )
    .unwrap();

    let (status, events) = pull_now(home.path(), 39).await;
    let spool = Spool::new(home.path()).unwrap();
    assert!(
        spool.get(Dir::Asks, &qb.id).unwrap().is_none(),
        "B's ask left asks/"
    );
    assert_eq!(
        spool.get(Dir::Done, &qb.id).unwrap().unwrap().state,
        "declined"
    );
    assert_eq!(
        spool.get(Dir::Asks, &qc.id).unwrap().unwrap().state,
        "waiting",
        "C's ask is untouched"
    );
    assert!(spool.get(Dir::Done, &qc.id).unwrap().is_none());
    assert_eq!(status.open_asks, 1);
    assert_eq!(status.peers_probed, 2);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(
        events[0],
        owlpost::server::DaemonEvent::Declined(owlpost::server::Declined {
            id: qb.id.clone(),
            peer: b.fp(),
        })
    );
    // Each peer was asked about its own ask, and about no other.
    let log = log_text();
    let declined = log
        .lines()
        .find(|l| l.contains("question declined") && l.contains(&qb.id))
        .unwrap_or_else(|| panic!("no 'question declined' line for {}:\n{log}", qb.id));
    assert!(declined.contains(&b.fp()), "{declined}");
    let held = log
        .lines()
        .find(|l| l.contains("task state") && l.contains(&qc.id))
        .unwrap_or_else(|| panic!("no 'task state' line for {}:\n{log}", qc.id));
    assert!(held.contains(&c.fp()), "{held}");
    assert!(held.contains(envelope::TASK_STATE_SUBMITTED), "{held}");
    assert!(
        !log.lines()
            .any(|l| l.contains(&c.fp()) && l.contains(&qb.id)),
        "C was asked about B's ask:\n{log}"
    );
    assert!(
        !log.lines()
            .any(|l| l.contains(&b.fp()) && l.contains(&qc.id)),
        "B was asked about C's ask:\n{log}"
    );
    b.running.shutdown();
    c.running.shutdown();
}

// ---- a peer whose Task route fails ---------------------------------------------------------
// The real daemon never answers `GET /v1/questions/{id}` with a `5xx`, so the `Err` arm of the
// Task fetch needs a scripted peer: A's pinned mTLS, an empty outbox (the answer half of the
// pull succeeds, so the peer really is reached) and a `500` for every Task fetch, counted
// server-side.

/// What `client::fetch_task` turns the peer's `500` into (`error_message`: the status line
/// plus the JSON `error` field).
const TASK_FAILURE: &str = "peer returned 500 Internal Server Error: the task store is down";

struct FailingTaskPeer {
    id: Identity,
    addr: SocketAddr,
    fetches: Arc<AtomicUsize>,
    handle: Handle<SocketAddr>,
}

impl FailingTaskPeer {
    fn fetches(&self) -> usize {
        self.fetches.load(Ordering::SeqCst)
    }
    fn shutdown(&self) {
        self.handle.shutdown();
    }
}

async fn empty_outbox() -> Json<serde_json::Value> {
    Json(serde_json::json!([]))
}

async fn failing_task(
    State(fetches): State<Arc<AtomicUsize>>,
    AxPath(_id): AxPath<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    fetches.fetch_add(1, Ordering::SeqCst);
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": "the task store is down" })),
    )
}

/// A peer with `seed`'s identity that admits only `asker`'s key, serves an empty outbox and
/// fails every Task fetch.
async fn spawn_failing_task_peer(seed: u8, asker: &Identity) -> FailingTaskPeer {
    let id = id(seed);
    let allowed = [common::key(asker)].into_iter().collect();
    let cfg = owlpost::tls::server_config(&id, allowed, false).unwrap();
    let fetches = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/v1/outbox", get(empty_outbox))
        .route("/v1/questions/{id}", get(failing_task))
        .with_state(Arc::clone(&fetches));
    let handle = Handle::new();
    let server = axum_server::bind("127.0.0.1:0".parse().unwrap())
        .acceptor(RustlsAcceptor::new(RustlsConfig::from_config(cfg)))
        .handle(handle.clone());
    tokio::spawn(server.serve(app.into_make_service()));
    let addr = handle.listening().await.expect("failing peer bound");
    FailingTaskPeer {
        id,
        addr,
        fetches,
        handle,
    }
}

/// The `Err` arm: unlike `unreached_peer_is_not_asked_for_a_task`, the peer is UP and the
/// fetch really happens — and fails. The ask stays open, nothing is reported, and the failure
/// is logged.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failing_task_route_leaves_the_ask_open_and_is_logged() {
    logs();
    let a = id(43);
    let peer = spawn_failing_task_peer(44, &a).await;
    let home = asker_home(&a, &peer.id, &peer.addr.to_string());
    let q = file_ask(home.path(), &a, &peer.id, "does the task route work?");

    let (status, events) = pull_now(home.path(), 43).await;
    let spool = Spool::new(home.path()).unwrap();
    assert_eq!(
        spool.get(Dir::Asks, &q.id).unwrap().unwrap().state,
        "waiting"
    );
    assert!(spool.get(Dir::Done, &q.id).unwrap().is_none());
    assert_eq!(status.open_asks, 1);
    assert_eq!(status.peers_probed, 1);
    assert!(events.is_empty(), "{events:?}");
    assert_eq!(peer.fetches(), 1, "the peer was reached and asked once");
    let log = log_text();
    let line = log
        .lines()
        .find(|l| l.contains("task fetch failed") && l.contains(&q.id))
        .unwrap_or_else(|| panic!("no 'task fetch failed' line for {}:\n{log}", q.id));
    assert!(line.contains(&fp(&peer.id)), "{line}");
    assert!(line.contains(TASK_FAILURE), "{line}");
    peer.shutdown();
}
