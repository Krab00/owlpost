//! OWL-008: the daemon's pull loop. Two in-process daemons, A (asker) and B (responder), with
//! A's `pull_interval_secs = 1`. Answers are placed straight into B's `outbox/` (what `owl
//! send` does) and A's loop is expected to verify, ingest, ack and report them.
//!
//! AC1 `answer_is_pulled_and_acked`, AC2 `forged_answer_is_dropped`, AC3
//! `unrelated_answer_is_ignored`, AC4 `offline_responder_is_skipped_then_retried`, AC5
//! `outbox_ttl_expires`, AC6 `status_file_is_written_each_loop`.

mod common;

use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use owlpost::contacts::Mode;
use owlpost::envelope::{self, Envelope, Kind, Payload};
use owlpost::identity::Identity;
use owlpost::pull::{PullStatus, STATUS_FILE, read_status};
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
    let hash = envelope::question_hash(common::PROJECT, common::PATH, text);
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
    let hash = envelope::question_hash(common::PROJECT, common::PATH, text);
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
    let b = spawn_b(&a.id).await;
    let dead = closed_port();
    point_at(&a, &b.id, &dead);
    let q = open_ask(&a, &b.id, "are you there?");
    let ans = outbox_answer(&b, &b.id, &q, ANSWER, None);
    let started = Instant::now();

    // For the first 3 s the endpoint is closed: the probe fails and is recorded as such.
    wait_for_log(Duration::from_secs(3), "outbox fetch failed");
    wait_until(
        Duration::from_secs(3),
        "status with the peer probed",
        || status(a.home()).is_some_and(|s| s.open_asks == 1 && s.peers_probed == 1),
    );
    assert!(
        a.spool().get(Dir::Inbox, &ans.id).unwrap().is_none(),
        "nothing ingested while offline"
    );
    std::thread::sleep(Duration::from_secs(3).saturating_sub(started.elapsed()));

    // "Start B": point A's contact at the live listener; the next pull picks up the new book.
    point_at(&a, &b.id, &b.addr.to_string());
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
    assert_ingested(&a, &b, &q, &ans, "are you there?");
    let log = log_text();
    assert!(
        log.lines()
            .any(|l| l.contains("outbox fetch failed") && l.contains(&dead)),
        "the closed port was reported:\n{log}"
    );
    a.running.shutdown();
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
    // A record from this second is exactly at the TTL boundary and must stay.
    let fresh = Payload::answer(&q, "fresh", "fake", 0, false);
    let mut rec = record(&Envelope::sign(&fresh, &b), "unacked");
    rec.received_at = envelope::unix_to_rfc3339(now);
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
