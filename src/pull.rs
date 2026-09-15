//! Pull loop (architecture §3.5): every `pull_interval_secs` the daemon groups `asks/` by
//! responder, fetches each responder's outbox, verifies and ingests the answers to its open
//! asks, acks them, expires its own stale outbox entries and writes `daemon.status`.
//!
//! `owl ask --wait` runs the same ingestion (`ingest_envelope`) for a single ask.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::client::{self, Iroh};
use crate::config::Config;
use crate::contacts::{Contact, ContactBook};
use crate::envelope::{self, Body, Envelope, Kind, Payload};
use crate::identity::Identity;
use crate::server::{AnswerIngested, AppState, DaemonEvent, Declined, on_pull_event};
use crate::spool::{Dir, Record, Spool};

/// `$OWLPOST_HOME/daemon.status`, rewritten after every loop.
pub const STATUS_FILE: &str = "daemon.status";
/// How long a responder whose last probe failed is left alone.
pub const PROBE_SKIP: Duration = Duration::from_secs(60);

/// Written after each loop so `owl doctor` can report the last pull.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullStatus {
    /// RFC 3339 UTC time the loop finished.
    pub last_pull_at: String,
    /// Asks still waiting after this loop.
    pub open_asks: usize,
    /// Responders contacted this loop (reachable or not); skipped ones are not counted.
    pub peers_probed: usize,
}

/// Outcome of the last probe of one responder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    pub last_probe: Instant,
    pub ok: bool,
}

/// In-memory liveness cache: a responder whose last probe failed is skipped for `skip`.
#[derive(Debug)]
pub struct Liveness {
    skip: Duration,
    map: HashMap<String, Probe>,
}

impl Liveness {
    pub fn new(skip: Duration) -> Liveness {
        Liveness {
            skip,
            map: HashMap::new(),
        }
    }

    /// The daemon's skip window: 60 s, capped at one pull interval, minus one second so the
    /// re-probe never depends on tick jitter (two ticks are at least `interval` apart, and
    /// `now` is sampled a little after each one).
    // ponytail: `PROBE_SKIP` capped at `interval - 1 s` — with a 1 s interval an offline peer
    // is retried every loop, with the default 60 s interval on the next loop. Raise the cap
    // (or count loops instead of seconds) if short intervals should back off further.
    pub fn skip_for(interval: Duration) -> Duration {
        PROBE_SKIP
            .min(interval)
            .saturating_sub(Duration::from_secs(1))
    }

    /// True while the last probe failed less than `skip` ago; a peer never probed, or whose
    /// last probe succeeded, is never skipped.
    pub fn should_skip(&self, fingerprint: &str, now: Instant) -> bool {
        self.map
            .get(fingerprint)
            .is_some_and(|p| !p.ok && now.duration_since(p.last_probe) < self.skip)
    }

    pub fn record(&mut self, fingerprint: &str, ok: bool, at: Instant) {
        self.map
            .insert(fingerprint.to_string(), Probe { last_probe: at, ok });
    }

    pub fn get(&self, fingerprint: &str) -> Option<Probe> {
        self.map.get(fingerprint).copied()
    }
}

/// What the loop needs from an `asks/` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAsk {
    pub id: String,
    /// Responder fingerprint: `meta.peer`, else the question's `to`.
    pub peer: String,
    /// Asker-cache key: `meta.hash`, else recomputed from the question.
    pub hash: String,
    /// Path the question was about (for the notification); `-` for a repo-level question.
    pub path: String,
    /// The question carried a context snippet or continued a thread (`meta.threaded`,
    /// OWL-034): its answer is stored but never written to the asker cache.
    pub threaded: bool,
}

/// Parses one `asks/` record; `None` (warned) when `raw` is not a question payload.
pub fn open_ask(id: &str, rec: &Record) -> Option<OpenAsk> {
    let payload: Payload = match serde_json::from_str(&rec.raw) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(id, error = %e, "skipping ask: raw is not a payload");
            return None;
        }
    };
    // OWL-039: a content ask has no question hash at all — it is keyed by ref and by the
    // owner's consent — so it carries an empty one and is never cached in either direction.
    let question = match &payload.body {
        Body::Question {
            project,
            path,
            question,
            ..
        } => Some((project.clone(), path.clone(), question.clone())),
        Body::Content { .. } if payload.kind == crate::envelope::Kind::Content => None,
        // OWL-040: a tool-call ask has no question hash either, for the same reason.
        Body::ToolCall { .. } if payload.kind == crate::envelope::Kind::ToolCall => None,
        _ => {
            tracing::warn!(id, "skipping ask: payload is not a question");
            return None;
        }
    };
    let meta_str = |key: &str| {
        rec.meta
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    Some(OpenAsk {
        id: id.to_string(),
        peer: meta_str("peer").unwrap_or_else(|| payload.to.clone()),
        hash: meta_str("hash").unwrap_or_else(|| match &question {
            Some((project, path, question)) => {
                envelope::question_hash(project, path.as_deref(), question)
            }
            None => String::new(),
        }),
        path: match (&question, &payload.body) {
            (Some((_, path, _)), _) => path.clone(),
            (None, Body::Content { path, .. }) => path.clone(),
            (None, _) => None,
        }
        .unwrap_or_else(|| "-".to_string()),
        threaded: rec
            .meta
            .get("threaded")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// Every parseable record in `asks/`, keyed by id; corrupt files are warned about and skipped.
pub fn open_asks(spool: &Spool) -> anyhow::Result<BTreeMap<String, OpenAsk>> {
    Ok(spool
        .list_lenient(Dir::Asks)?
        .iter()
        .filter_map(|(id, rec)| open_ask(id, rec))
        .map(|ask| (ask.id.clone(), ask))
        .collect())
}

/// Verified answer → `inbox/<answer id>` (state `pending`, unseen, `meta = {peer, hash,
/// in_reply_to}`) and, when `cache` (the question was not threaded, OWL-034), the asker
/// cache under `hash`.
pub fn store_answer(
    spool: &Spool,
    env: &Envelope,
    answer: &Payload,
    peer: &str,
    hash: &str,
    question_id: &str,
    cache: bool,
) -> anyhow::Result<()> {
    let mut rec = Record {
        raw: env.raw.clone(),
        sig: env.sig.clone(),
        state: "pending".into(),
        seen: false,
        received_at: envelope::rfc3339_now(),
        draft: None,
        meta: json!({ "peer": peer, "hash": hash, "in_reply_to": question_id }),
    };
    // Record birth (OWL-038): the peer's answer landing here. OWL-039: a content reply
    // names its own kind, so `owl thread` shows the content leg of the exchange.
    let kind = match answer.kind {
        Kind::ContentReply => "content-received",
        Kind::ToolReply => "tool-received",
        _ => "answer-received",
    };
    crate::events::push(&mut rec, kind, None, None);
    spool.put(Dir::Inbox, &answer.id, &rec)?;
    if cache {
        spool.cache_put(hash, &rec)?;
    }
    Ok(())
}

/// An answer that went through `ingest_envelope`.
#[derive(Debug)]
pub struct Ingested {
    pub ask: OpenAsk,
    pub answer: Payload,
    /// `Some` when the ack did not reach the responder; the answer is stored either way.
    pub ack_error: Option<anyhow::Error>,
}

/// Per-envelope verdict of `ingest_envelope`.
#[derive(Debug)]
pub enum Verdict {
    Ingested(Box<Ingested>),
    /// Signature (or shape) did not verify against the responder's key: warned, not acked.
    Forged,
    /// Verified, but `in_reply_to` is none of `open`: left on the responder, not acked.
    Unrelated,
}

/// One outbox envelope from `contact` against the open asks: verify with the pinned key,
/// match `in_reply_to` against the asks addressed to *this* contact (an answer B signs to a
/// question asked of C is `Unrelated`), store in `inbox/` + cache, move the ask to `done/`
/// (`answered`), ack.
///
/// The inbox write comes before the ask move so a crash in between leaves the ask open and
/// the (idempotent) inbox write to be repeated on the next pull.
// ponytail: an answer whose ack failed is re-served by the responder until its TTL, and every
// later pull sees it as `Unrelated` (the ask is closed by then) — re-ack from `done/` if that
// churn ever matters.
pub fn ingest_envelope(
    identity: &Identity,
    contact: &Contact,
    iroh: &Iroh,
    spool: &Spool,
    open: &BTreeMap<String, OpenAsk>,
    env: &Envelope,
) -> anyhow::Result<Verdict> {
    let answer = match client::verify_answer(contact, env, None) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                peer = %contact.fingerprint,
                claimed_id = %claimed_id(env),
                error = %format!("{e:#}"),
                "dropping outbox entry: signature does not verify"
            );
            return Ok(Verdict::Forged);
        }
    };
    let ask = answer
        .in_reply_to
        .as_deref()
        .and_then(|q| open.get(q))
        .filter(|ask| ask.peer == contact.fingerprint);
    let Some(ask) = ask else {
        tracing::debug!(
            peer = %contact.fingerprint,
            id = %answer.id,
            in_reply_to = ?answer.in_reply_to,
            "outbox entry replies to no open ask to this peer; left unacked"
        );
        return Ok(Verdict::Unrelated);
    };
    // OWL-039: a content reply never enters the asker's cache — the cache answers questions
    // by their text, and content is keyed by ref and by the owner's consent.
    let cache = !ask.threaded && answer.kind == Kind::Answer && !ask.hash.is_empty();
    store_answer(
        spool,
        env,
        &answer,
        &contact.fingerprint,
        &ask.hash,
        &ask.id,
        cache,
    )?;
    spool.move_to(Dir::Asks, &ask.id, Dir::Done)?;
    spool.set_state_with_event(
        Dir::Done,
        &ask.id,
        "answered",
        "answer-received",
        None,
        None,
    )?;
    let ack_error = client::ack(identity, contact, iroh, &answer.id).err();
    Ok(Verdict::Ingested(Box::new(Ingested {
        ask: ask.clone(),
        answer,
        ack_error,
    })))
}

/// The `id` an unverified envelope claims, for the log line only.
fn claimed_id(env: &Envelope) -> String {
    serde_json::from_str::<Value>(&env.raw)
        .ok()
        .and_then(|v| v.get("id").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| "?".to_string())
}

/// Records the peer's Task state on the still-open ask `id` (OWL-038): one `state-seen`
/// event, collapsed by `events::push` when the state has not moved since the last one.
/// Best effort — the ask may have been closed between the fetch and this write.
pub fn note_task_state(spool: &Spool, id: &str, task: &client::Task) {
    if let Err(e) = spool.push_event(
        Dir::Asks,
        id,
        crate::events::STATE_SEEN,
        Some("peer"),
        Some(json!({ "state": task.state, "text": task.text })),
    ) {
        tracing::debug!(id, error = %format!("{e:#}"), "recording the peer's task state failed");
    }
}

/// Closes ask `id` as declined (OWL-034): the `asks/` record moves to `done/` with state
/// `declined`. Move first, like the ack handler, so a failed move leaves the ask open.
pub fn close_declined(spool: &Spool, id: &str) -> anyhow::Result<()> {
    spool.move_to(Dir::Asks, id, Dir::Done)?;
    spool.set_state_with_event(Dir::Done, id, "declined", "declined", Some("peer"), None)
}

/// One pull over every responder with an open ask (iroh first, then `endpoints`, see
/// `client`). Contacts are reloaded on each call so an endpoint edit is picked up without
/// a restart. For every ask still open to a responder that was reached, the peer's Task
/// is fetched too (OWL-034): a `REJECTED` one closes the ask as `done/declined` and
/// reports `DaemonEvent::Declined`. Returns the status to write.
#[allow(clippy::too_many_arguments)]
pub fn pull_once(
    home: &Path,
    cwd: &Path,
    identity: &Identity,
    iroh: &Iroh,
    spool: &Spool,
    liveness: &mut Liveness,
    now: Instant,
    mut on_event: impl FnMut(DaemonEvent),
) -> anyhow::Result<PullStatus> {
    let mut open = open_asks(spool)?;
    let book = ContactBook::load(home, cwd)?;
    let mut by_peer: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for ask in open.values() {
        by_peer
            .entry(ask.peer.clone())
            .or_default()
            .push(ask.id.clone());
    }
    let mut peers_probed = 0;
    for peer in by_peer.keys() {
        let Some(contact) = book.contacts.iter().find(|c| &c.fingerprint == peer) else {
            tracing::warn!(peer, "open ask to an unknown contact; not pulled");
            continue;
        };
        if liveness.should_skip(peer, now) {
            tracing::debug!(peer, "responder offline recently; skipped");
            continue;
        }
        peers_probed += 1;
        let items = match client::fetch_outbox(identity, contact, iroh) {
            Ok(items) => {
                liveness.record(peer, true, now);
                tracing::debug!(peer, count = items.len(), "outbox fetched");
                items
            }
            Err(e) => {
                liveness.record(peer, false, now);
                tracing::warn!(peer, error = %format!("{e:#}"), "outbox fetch failed");
                continue;
            }
        };
        for env in &items {
            match ingest_envelope(identity, contact, iroh, spool, &open, env) {
                Ok(Verdict::Ingested(ing)) => {
                    if let Some(e) = &ing.ack_error {
                        tracing::warn!(peer, id = %ing.answer.id, error = %format!("{e:#}"), "ack failed");
                    }
                    // The pulled answer is a new inbox record: wake one session (OWL-033).
                    match Config::load(home)
                        .and_then(|cfg| crate::route::route(home, &cfg, spool, &ing.answer.id))
                    {
                        Ok(Some(sid)) => {
                            tracing::info!(id = %ing.answer.id, session = %sid, "routed to session")
                        }
                        Ok(None) => tracing::debug!(id = %ing.answer.id, "no live session to wake"),
                        Err(e) => {
                            tracing::warn!(id = %ing.answer.id, error = %format!("{e:#}"), "routing failed")
                        }
                    }
                    open.remove(&ing.ask.id);
                    on_event(DaemonEvent::Answer(AnswerIngested {
                        id: ing.ask.id.clone(),
                        peer: peer.clone(),
                        path: ing.ask.path.clone(),
                    }));
                }
                Ok(Verdict::Forged | Verdict::Unrelated) => {}
                Err(e) => {
                    tracing::warn!(peer, error = %format!("{e:#}"), "ingesting answer failed");
                }
            }
        }
        // The responder is online: ask it where every still-open ask stands (OWL-034). A
        // fetch error is logged and the ask stays open; only `REJECTED` closes it.
        let still_open: Vec<String> = open
            .values()
            .filter(|a| &a.peer == peer)
            .map(|a| a.id.clone())
            .collect();
        for id in still_open {
            match client::fetch_task(identity, contact, iroh, &id) {
                Ok(Some(task)) if task.rejected() => match close_declined(spool, &id) {
                    Ok(()) => {
                        tracing::info!(peer, id, text = %task.text, "question declined");
                        open.remove(&id);
                        on_event(DaemonEvent::Declined(Declined {
                            id,
                            peer: peer.clone(),
                        }));
                    }
                    Err(e) => {
                        tracing::warn!(peer, id, error = %format!("{e:#}"), "closing declined ask failed")
                    }
                },
                Ok(Some(task)) => {
                    note_task_state(spool, &id, &task);
                    tracing::debug!(peer, id, state = %task.state, "task state");
                }
                Ok(None) => tracing::debug!(peer, id, "peer holds no task for the ask"),
                Err(e) => tracing::warn!(peer, id, error = %format!("{e:#}"), "task fetch failed"),
            }
        }
    }
    Ok(PullStatus {
        last_pull_at: envelope::rfc3339_now(),
        open_asks: open.len(),
        peers_probed,
    })
}

/// Moves every `outbox/` record older than `ttl_days` (strictly: age > TTL) to `done/` with
/// state `expired`. Returns how many moved. A record whose `received_at` does not parse is
/// kept and warned about.
pub fn expire_outbox(spool: &Spool, ttl_days: u64, now_unix: u64) -> anyhow::Result<usize> {
    let ttl_secs = ttl_days.saturating_mul(86_400);
    let mut expired = 0;
    for (id, rec) in spool.list_lenient(Dir::Outbox)? {
        let Some(received) = envelope::parse_rfc3339_to_unix(&rec.received_at) else {
            tracing::warn!(id, received_at = %rec.received_at, "outbox record has no usable received_at; kept");
            continue;
        };
        if now_unix.saturating_sub(received) <= ttl_secs {
            continue;
        }
        // Move first, like the ack handler: a failed move must not leave `expired` in outbox/.
        match spool.move_to(Dir::Outbox, &id, Dir::Done).and_then(|_| {
            spool.set_state_with_event(Dir::Done, &id, "expired", "expired", None, None)
        }) {
            Ok(()) => {
                tracing::info!(id, "outbox entry expired");
                expired += 1;
            }
            Err(e) => tracing::warn!(id, error = %format!("{e:#}"), "expiring outbox entry failed"),
        }
    }
    Ok(expired)
}

/// Atomically writes `<home>/daemon.status` (temp file + rename).
pub fn write_status(home: &Path, status: &PullStatus) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(status).context("serialising daemon.status")?;
    bytes.push(b'\n');
    crate::daemon::write_atomic(home, STATUS_FILE, &bytes)
}

/// Parses `<home>/daemon.status`; `Ok(None)` when the file does not exist.
pub fn read_status(home: &Path) -> anyhow::Result<Option<PullStatus>> {
    let path = home.join(STATUS_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// The loop period: `pull_interval_secs`, floored at 1 s (a zero interval would spin).
pub fn loop_interval(config: &Config) -> Duration {
    Duration::from_secs(config.pull_interval_secs.max(1))
}

/// One loop iteration: pull, expire, write the status file. Never panics on I/O.
pub fn tick(state: &AppState, liveness: &mut Liveness) {
    match expire_outbox(
        &state.spool,
        state.config.outbox_ttl_days,
        envelope::now_unix(),
    ) {
        Ok(0) => {}
        Ok(n) => tracing::info!(count = n, "expired outbox entries"),
        Err(e) => tracing::warn!(error = %format!("{e:#}"), "outbox expiry failed"),
    }
    let status = match pull_once(
        &state.home,
        &state.cwd,
        &state.identity,
        &Iroh::for_daemon(state),
        &state.spool,
        liveness,
        Instant::now(),
        |ev| on_pull_event(state, ev),
    ) {
        Ok(status) => status,
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "pull failed");
            return;
        }
    };
    tracing::debug!(
        open_asks = status.open_asks,
        peers_probed = status.peers_probed,
        "pulled"
    );
    if let Err(e) = write_status(&state.home, &status) {
        tracing::warn!(error = %format!("{e:#}"), "writing daemon.status failed");
    }
}

/// Runs `tick` immediately and then every `loop_interval`, off the async runtime (the HTTP
/// client is blocking). Ends only when the task is aborted.
pub async fn run_loop(state: Arc<AppState>) {
    let interval = loop_interval(&state.config);
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut liveness = Liveness::new(Liveness::skip_for(interval));
    loop {
        ticker.tick().await;
        let st = state.clone();
        let mut lv = liveness;
        liveness = match tokio::task::spawn_blocking(move || {
            tick(&st, &mut lv);
            lv
        })
        .await
        {
            Ok(lv) => lv,
            Err(e) => {
                tracing::error!(error = %e, "pull tick panicked; liveness cache reset");
                Liveness::new(Liveness::skip_for(interval))
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity;

    fn rec(env: &Envelope, state: &str, received_at: &str, meta: Value) -> Record {
        Record {
            raw: env.raw.clone(),
            sig: env.sig.clone(),
            state: state.into(),
            seen: false,
            received_at: received_at.into(),
            draft: None,
            meta,
        }
    }

    fn question(a: &Identity, b: &Identity) -> Payload {
        Payload::question(
            &identity::fingerprint(&a.verifying_key()),
            &identity::fingerprint(&b.verifying_key()),
            "proj",
            Some("src/x.rs"),
            "why?",
        )
    }

    fn write_contact(home: &Path, id: &Identity, name: &str, endpoints: &[&str]) {
        let dir = home.join("contacts");
        std::fs::create_dir_all(&dir).unwrap();
        let v = json!({
            "name": name,
            "emails": [],
            "pubkey": identity::pubkey_string(&id.verifying_key()),
            "endpoints": endpoints,
            "source": "global",
        });
        std::fs::write(
            dir.join(format!(
                "{}.json",
                identity::fingerprint(&id.verifying_key())
            )),
            serde_json::to_vec(&v).unwrap(),
        )
        .unwrap();
    }

    fn closed_port() -> String {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().to_string()
    }

    fn no_iroh() -> Iroh {
        Iroh::Unavailable("no endpoint".into())
    }

    #[test]
    fn liveness_skips_only_recently_failed_peers() {
        let t0 = Instant::now();
        let mut lv = Liveness::new(Duration::from_secs(60));
        assert!(!lv.should_skip("owl:b", t0), "never probed");
        lv.record("owl:b", false, t0);
        assert!(lv.should_skip("owl:b", t0));
        assert!(lv.should_skip("owl:b", t0 + Duration::from_secs(59)));
        assert!(
            !lv.should_skip("owl:b", t0 + Duration::from_secs(60)),
            "exactly the window: probe again"
        );
        assert!(!lv.should_skip("owl:c", t0), "another peer is unaffected");
        lv.record("owl:b", true, t0 + Duration::from_secs(60));
        assert!(
            !lv.should_skip("owl:b", t0 + Duration::from_secs(61)),
            "a successful probe is never skipped"
        );
        assert_eq!(
            lv.get("owl:b"),
            Some(Probe {
                last_probe: t0 + Duration::from_secs(60),
                ok: true
            })
        );
        assert_eq!(lv.get("owl:zzz"), None);
        // A shorter window shortens the skip.
        let mut short = Liveness::new(Duration::from_secs(1));
        short.record("owl:b", false, t0);
        assert!(short.should_skip("owl:b", t0 + Duration::from_millis(999)));
        assert!(!short.should_skip("owl:b", t0 + Duration::from_secs(1)));
    }

    #[test]
    fn skip_window_is_one_second_short_of_the_interval() {
        assert_eq!(
            Liveness::skip_for(Duration::from_secs(1)),
            Duration::ZERO,
            "1 s interval: re-probed on every loop"
        );
        assert_eq!(
            Liveness::skip_for(Duration::from_secs(5)),
            Duration::from_secs(4)
        );
        assert_eq!(
            Liveness::skip_for(Duration::from_secs(60)),
            Duration::from_secs(59)
        );
        assert_eq!(
            Liveness::skip_for(Duration::from_secs(600)),
            Duration::from_secs(59),
            "never longer than PROBE_SKIP - 1 s"
        );
        assert_eq!(PROBE_SKIP, Duration::from_secs(60));
        // A zero window never skips, even right after a failure.
        let mut lv = Liveness::new(Duration::ZERO);
        let t0 = Instant::now();
        lv.record("owl:b", false, t0);
        assert!(!lv.should_skip("owl:b", t0));
    }

    #[test]
    fn loop_interval_floors_at_one_second() {
        let cfg = Config {
            pull_interval_secs: 0,
            ..Default::default()
        };
        assert_eq!(loop_interval(&cfg), Duration::from_secs(1));
        let cfg = Config {
            pull_interval_secs: 7,
            ..Default::default()
        };
        assert_eq!(loop_interval(&cfg), Duration::from_secs(7));
        assert_eq!(loop_interval(&Config::default()), Duration::from_secs(60));
    }

    #[test]
    fn open_ask_reads_meta_and_falls_back_to_the_payload() {
        let (a, b) = (Identity::from_seed([1; 32]), Identity::from_seed([2; 32]));
        let q = question(&a, &b);
        let env = Envelope::sign(&q, &a);
        let full = rec(
            &env,
            "waiting",
            "2026-09-01T00:00:00Z",
            json!({ "peer": "owl:override", "hash": "h1" }),
        );
        let ask = open_ask("q1", &full).unwrap();
        assert_eq!(
            ask,
            OpenAsk {
                id: "q1".into(),
                peer: "owl:override".into(),
                hash: "h1".into(),
                path: "src/x.rs".into(),
                threaded: false,
            }
        );
        // OWL-034: `meta.threaded` is read as a bool; anything else means not threaded.
        let threaded = rec(
            &env,
            "waiting",
            "2026-09-01T00:00:00Z",
            json!({ "peer": "owl:override", "hash": "h1", "threaded": true }),
        );
        assert!(open_ask("q1", &threaded).unwrap().threaded);
        let odd = rec(&env, "waiting", "x", json!({ "threaded": "yes" }));
        assert!(!open_ask("q1", &odd).unwrap().threaded);
        // No meta at all: peer from `to`, hash recomputed.
        let bare = rec(&env, "waiting", "2026-09-01T00:00:00Z", Value::Null);
        let ask = open_ask("q1", &bare).unwrap();
        assert_eq!(ask.peer, identity::fingerprint(&b.verifying_key()));
        assert_eq!(
            ask.hash,
            envelope::question_hash("proj", Some("src/x.rs"), "why?")
        );
        // Wrong-shape meta (array, empty strings, numbers): same fallbacks, no panic.
        for meta in [
            json!([1, 2]),
            json!({ "peer": "", "hash": 7 }),
            json!("str"),
        ] {
            let ask = open_ask("q1", &rec(&env, "waiting", "x", meta)).unwrap();
            assert_eq!(ask.peer, identity::fingerprint(&b.verifying_key()));
            assert_eq!(
                ask.hash,
                envelope::question_hash("proj", Some("src/x.rs"), "why?")
            );
        }
        // OWL-018: a repo-level question hashes over the empty path and shows `-`.
        let mut no_path = q.clone();
        no_path.body = Body::Question {
            project: "proj".into(),
            path: None,
            question: "why?".into(),
            context: None,
        };
        let env_np = Envelope::sign(&no_path, &a);
        let ask = open_ask("q2", &rec(&env_np, "waiting", "x", Value::Null)).unwrap();
        assert_eq!(ask.path, "-");
        assert_eq!(ask.hash, envelope::question_hash("proj", None, "why?"));
        assert_ne!(
            ask.hash,
            envelope::question_hash("proj", Some("src/x.rs"), "why?")
        );
        // Not a payload, or an answer payload: skipped.
        let garbage = Record {
            raw: "{not json".into(),
            ..bare.clone()
        };
        assert!(open_ask("q1", &garbage).is_none());
        let ans = Envelope::sign(&Payload::answer(&q, "yes", "fake", 0, false), &b);
        assert!(open_ask("q1", &rec(&ans, "waiting", "x", Value::Null)).is_none());
    }

    #[test]
    fn open_asks_skips_corrupt_files_and_keeps_the_rest() {
        let home = tempfile::tempdir().unwrap();
        let spool = Spool::new(home.path()).unwrap();
        let (a, b) = (Identity::from_seed([1; 32]), Identity::from_seed([2; 32]));
        let q = question(&a, &b);
        let env = Envelope::sign(&q, &a);
        spool
            .put(
                Dir::Asks,
                &q.id,
                &rec(&env, "waiting", "2026-09-01T00:00:00Z", Value::Null),
            )
            .unwrap();
        std::fs::write(spool.path(Dir::Asks, "broken"), "{").unwrap();
        std::fs::write(spool.path(Dir::Asks, "notes").with_extension("txt"), "x").unwrap();
        let open = open_asks(&spool).unwrap();
        assert_eq!(open.keys().collect::<Vec<_>>(), [&q.id]);
        assert!(spool.path(Dir::Asks, "broken").exists(), "left in place");
        // Missing asks/ dir: an error, not a panic.
        std::fs::remove_dir_all(home.path().join("spool/asks")).unwrap();
        assert!(open_asks(&spool).is_err());
    }

    #[test]
    fn expire_outbox_boundary() {
        let home = tempfile::tempdir().unwrap();
        let spool = Spool::new(home.path()).unwrap();
        let (a, b) = (Identity::from_seed([1; 32]), Identity::from_seed([2; 32]));
        let q = question(&a, &b);
        let mk = |suffix: &str, received_at: &str| {
            let ans = Payload::answer(&q, suffix, "fake", 0, false);
            let env = Envelope::sign(&ans, &b);
            spool
                .put(
                    Dir::Outbox,
                    &ans.id,
                    &rec(&env, "unacked", received_at, Value::Null),
                )
                .unwrap();
            ans.id
        };
        // now = 2026-09-02T00:00:00Z; TTL 1 day.
        let now = envelope::parse_rfc3339_to_unix("2026-09-02T00:00:00Z").unwrap();
        let exactly = mk("exactly", "2026-09-01T00:00:00Z");
        let older = mk("older", "2026-08-31T23:59:59Z");
        let fresh = mk("fresh", "2026-09-01T12:00:00Z");
        let future = mk("future", "2026-09-03T00:00:00Z");
        let unparseable = mk("bad", "yesterday");
        assert_eq!(expire_outbox(&spool, 1, now).unwrap(), 1);
        assert!(spool.get(Dir::Outbox, &older).unwrap().is_none());
        assert_eq!(
            spool.get(Dir::Done, &older).unwrap().unwrap().state,
            "expired"
        );
        for kept in [&exactly, &fresh, &future, &unparseable] {
            let r = spool
                .get(Dir::Outbox, kept)
                .unwrap()
                .expect("still in outbox");
            assert_eq!(r.state, "unacked");
            assert!(spool.get(Dir::Done, kept).unwrap().is_none());
        }
        // TTL 0: anything older than "now" goes, a record dated now stays.
        let at_now = mk("now", "2026-09-02T00:00:00Z");
        assert_eq!(
            expire_outbox(&spool, 0, now).unwrap(),
            2,
            "`exactly` and `fresh`; `older` already went in the first pass"
        );
        assert!(spool.get(Dir::Outbox, &at_now).unwrap().is_some());
        assert!(spool.get(Dir::Outbox, &future).unwrap().is_some());
        assert!(spool.get(Dir::Outbox, &unparseable).unwrap().is_some());
        for gone in [&exactly, &fresh] {
            assert_eq!(
                spool.get(Dir::Done, gone).unwrap().unwrap().state,
                "expired"
            );
        }
        // Nothing left to expire: 0, and the non-default 14-day TTL keeps a 13-day record.
        assert_eq!(expire_outbox(&spool, 0, now).unwrap(), 0);
        let thirteen = mk("13d", "2026-08-20T00:00:00Z");
        let fifteen = mk("15d", "2026-08-18T00:00:00Z");
        assert_eq!(expire_outbox(&spool, 14, now).unwrap(), 1);
        assert!(spool.get(Dir::Outbox, &thirteen).unwrap().is_some());
        assert!(spool.get(Dir::Outbox, &fifteen).unwrap().is_none());
    }

    #[test]
    fn expire_outbox_failed_move_leaves_the_record_unacked() {
        let home = tempfile::tempdir().unwrap();
        let spool = Spool::new(home.path()).unwrap();
        let (a, b) = (Identity::from_seed([1; 32]), Identity::from_seed([2; 32]));
        let q = question(&a, &b);
        let ans = Payload::answer(&q, "old", "fake", 0, false);
        let env = Envelope::sign(&ans, &b);
        spool
            .put(
                Dir::Outbox,
                &ans.id,
                &rec(&env, "unacked", "2020-01-01T00:00:00Z", Value::Null),
            )
            .unwrap();
        // Block the destination with a non-empty directory.
        std::fs::create_dir_all(spool.path(Dir::Done, &ans.id).join("child")).unwrap();
        assert_eq!(expire_outbox(&spool, 0, envelope::now_unix()).unwrap(), 0);
        assert_eq!(
            spool.get(Dir::Outbox, &ans.id).unwrap().unwrap().state,
            "unacked"
        );
        // Corrupt outbox file: skipped, others still processed.
        std::fs::write(spool.path(Dir::Outbox, "junk"), "nope").unwrap();
        assert_eq!(expire_outbox(&spool, 0, envelope::now_unix()).unwrap(), 0);
        assert!(spool.path(Dir::Outbox, "junk").exists());
    }

    #[test]
    fn status_file_roundtrip_and_failure_paths() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(read_status(home.path()).unwrap(), None, "no file yet");
        let status = PullStatus {
            last_pull_at: "2026-09-02T10:00:00Z".into(),
            open_asks: 2,
            peers_probed: 1,
        };
        write_status(home.path(), &status).unwrap();
        let raw: Value =
            serde_json::from_slice(&std::fs::read(home.path().join(STATUS_FILE)).unwrap()).unwrap();
        assert_eq!(raw["last_pull_at"], "2026-09-02T10:00:00Z");
        assert_eq!(raw["open_asks"], 2);
        assert_eq!(raw["peers_probed"], 1);
        assert_eq!(read_status(home.path()).unwrap(), Some(status.clone()));
        assert!(!home.path().join("daemon.status.tmp").exists());
        // Unparseable file: an error naming the file.
        std::fs::write(home.path().join(STATUS_FILE), "{").unwrap();
        let err = read_status(home.path()).unwrap_err().to_string();
        assert!(err.contains("parsing"), "{err}");
        // Destination blocked by a non-empty directory: rename fails, no tmp left behind.
        std::fs::remove_file(home.path().join(STATUS_FILE)).unwrap();
        std::fs::create_dir_all(home.path().join(STATUS_FILE).join("child")).unwrap();
        let err = write_status(home.path(), &status).unwrap_err().to_string();
        assert!(err.contains("renaming to"), "{err}");
        assert!(!home.path().join("daemon.status.tmp").exists());
        let err = read_status(home.path()).unwrap_err().to_string();
        assert!(err.contains("reading"), "{err}");
    }

    /// `pull_once` against a closed port: the peer is probed once, recorded offline, then
    /// skipped inside the window; a malformed ask and an ask to an unknown contact are skipped
    /// without touching the rest.
    #[test]
    fn pull_once_records_offline_and_skips_bad_asks() {
        let home = tempfile::tempdir().unwrap();
        let (a, b, c) = (
            Identity::from_seed([1; 32]),
            Identity::from_seed([2; 32]),
            Identity::from_seed([3; 32]),
        );
        let spool = Spool::new(home.path()).unwrap();
        let port = closed_port();
        write_contact(home.path(), &b, "Bea", &[&port]);
        let fp_b = identity::fingerprint(&b.verifying_key());
        let fp_c = identity::fingerprint(&c.verifying_key());
        let q = question(&a, &b);
        spool
            .put(
                Dir::Asks,
                &q.id,
                &rec(
                    &Envelope::sign(&q, &a),
                    "waiting",
                    "2026-09-01T00:00:00Z",
                    json!({ "peer": fp_b, "hash": "h" }),
                ),
            )
            .unwrap();
        let to_c = question(&a, &c);
        spool
            .put(
                Dir::Asks,
                &to_c.id,
                &rec(
                    &Envelope::sign(&to_c, &a),
                    "waiting",
                    "2026-09-01T00:00:00Z",
                    Value::Null,
                ),
            )
            .unwrap();
        std::fs::write(spool.path(Dir::Asks, "junk"), "{\"raw\": 1}").unwrap();
        let mut lv = Liveness::new(Duration::from_secs(60));
        let t0 = Instant::now();
        let mut events = Vec::new();
        let status = pull_once(
            home.path(),
            home.path(),
            &a,
            &no_iroh(),
            &spool,
            &mut lv,
            t0,
            |ev| events.push(ev),
        )
        .unwrap();
        assert_eq!(status.open_asks, 2, "both parseable asks stay open");
        assert_eq!(status.peers_probed, 1, "only the known contact is probed");
        assert!(events.is_empty());
        assert_eq!(lv.get(&fp_b).map(|p| p.ok), Some(false));
        assert_eq!(lv.get(&fp_c), None, "unknown contact: never probed");
        assert!(envelope::parse_rfc3339_to_unix(&status.last_pull_at).is_some());
        // Inside the window the offline peer is skipped: nothing probed at all.
        let status = pull_once(
            home.path(),
            home.path(),
            &a,
            &no_iroh(),
            &spool,
            &mut lv,
            t0 + Duration::from_secs(59),
            |_| {},
        )
        .unwrap();
        assert_eq!(status.peers_probed, 0);
        // Past the window it is probed again.
        let status = pull_once(
            home.path(),
            home.path(),
            &a,
            &no_iroh(),
            &spool,
            &mut lv,
            t0 + Duration::from_secs(60),
            |_| {},
        )
        .unwrap();
        assert_eq!(status.peers_probed, 1);
        assert_eq!(
            lv.get(&fp_b).unwrap().last_probe,
            t0 + Duration::from_secs(60)
        );
        // Nothing to do at all: no probes, no error.
        let empty = tempfile::tempdir().unwrap();
        let spool = Spool::new(empty.path()).unwrap();
        let status = pull_once(
            empty.path(),
            empty.path(),
            &a,
            &no_iroh(),
            &spool,
            &mut Liveness::new(Duration::ZERO),
            t0,
            |_| {},
        )
        .unwrap();
        assert_eq!((status.open_asks, status.peers_probed), (0, 0));
    }

    #[test]
    fn ingest_envelope_verdicts() {
        let home = tempfile::tempdir().unwrap();
        let spool = Spool::new(home.path()).unwrap();
        let (a, b, other) = (
            Identity::from_seed([1; 32]),
            Identity::from_seed([2; 32]),
            Identity::from_seed([9; 32]),
        );
        let port = closed_port();
        write_contact(home.path(), &b, "Bea", &[&port]);
        let book = ContactBook::load(home.path(), home.path()).unwrap();
        let contact = book.contacts[0].clone();
        let q = question(&a, &b);
        spool
            .put(
                Dir::Asks,
                &q.id,
                &rec(
                    &Envelope::sign(&q, &a),
                    "waiting",
                    "2026-09-01T00:00:00Z",
                    json!({ "peer": contact.fingerprint, "hash": "h" }),
                ),
            )
            .unwrap();
        let open = open_asks(&spool).unwrap();
        let ans = Payload::answer(&q, "yes", "fake", 0, false);
        // Forged: signed by somebody else.
        let forged = Envelope::sign(&ans, &other);
        assert!(matches!(
            ingest_envelope(&a, &contact, &no_iroh(), &spool, &open, &forged).unwrap(),
            Verdict::Forged
        ));
        // Unrelated: replies to an unknown question.
        let mut unrelated = ans.clone();
        unrelated.in_reply_to = Some("someone-elses".into());
        let unrelated = Envelope::sign(&unrelated, &b);
        assert!(matches!(
            ingest_envelope(&a, &contact, &no_iroh(), &spool, &open, &unrelated).unwrap(),
            Verdict::Unrelated
        ));
        // B-signed answer to A's open ask to C: verified, but not B's to answer.
        let to_c = question(&a, &other);
        spool
            .put(
                Dir::Asks,
                &to_c.id,
                &rec(
                    &Envelope::sign(&to_c, &a),
                    "waiting",
                    "2026-09-01T00:00:00Z",
                    json!({ "peer": identity::fingerprint(&other.verifying_key()), "hash": "hc" }),
                ),
            )
            .unwrap();
        let open = open_asks(&spool).unwrap();
        assert_eq!(open.len(), 2);
        let hijack = Envelope::sign(&Payload::answer(&to_c, "mine now", "fake", 0, false), &b);
        assert!(matches!(
            ingest_envelope(&a, &contact, &no_iroh(), &spool, &open, &hijack).unwrap(),
            Verdict::Unrelated
        ));
        assert!(
            spool.get(Dir::Asks, &to_c.id).unwrap().is_some(),
            "ask to C stays open"
        );
        assert!(spool.cache_get("hc").unwrap().is_none());
        let mut no_reply = ans.clone();
        no_reply.in_reply_to = None;
        assert!(matches!(
            ingest_envelope(
                &a,
                &contact,
                &no_iroh(),
                &spool,
                &open,
                &Envelope::sign(&no_reply, &b)
            )
            .unwrap(),
            Verdict::Unrelated
        ));
        assert!(spool.get(Dir::Inbox, &ans.id).unwrap().is_none());
        assert!(spool.get(Dir::Asks, &q.id).unwrap().is_some());
        // Good: stored, ask moved, ack failed (closed port) but reported, not fatal.
        let good = Envelope::sign(&ans, &b);
        let Verdict::Ingested(ing) =
            ingest_envelope(&a, &contact, &no_iroh(), &spool, &open, &good).unwrap()
        else {
            panic!("expected Ingested");
        };
        assert_eq!(ing.ask.id, q.id);
        assert_eq!(ing.answer, ans);
        assert!(
            ing.ack_error
                .as_ref()
                .is_some_and(|e| e.to_string().starts_with("offline")),
            "{:?}",
            ing.ack_error
        );
        let inbox = spool.get(Dir::Inbox, &ans.id).unwrap().unwrap();
        assert_eq!(inbox.state, "pending");
        assert!(!inbox.seen);
        assert_eq!(inbox.meta["peer"], contact.fingerprint);
        assert_eq!(inbox.meta["hash"], "h");
        assert_eq!(inbox.meta["in_reply_to"], q.id);
        assert_eq!(spool.cache_get("h").unwrap().unwrap().raw, good.raw);
        assert!(spool.get(Dir::Asks, &q.id).unwrap().is_none());
        assert_eq!(
            spool.get(Dir::Done, &q.id).unwrap().unwrap().state,
            "answered"
        );
    }

    /// The ask move is blocked: the inbox record (written first) exists, the ask stays open
    /// and the error surfaces — the next pull repeats the idempotent write and retries.
    #[test]
    fn ingest_writes_inbox_before_moving_the_ask() {
        let home = tempfile::tempdir().unwrap();
        let spool = Spool::new(home.path()).unwrap();
        let (a, b) = (Identity::from_seed([1; 32]), Identity::from_seed([2; 32]));
        let port = closed_port();
        write_contact(home.path(), &b, "Bea", &[&port]);
        let contact = ContactBook::load(home.path(), home.path())
            .unwrap()
            .contacts[0]
            .clone();
        let q = question(&a, &b);
        spool
            .put(
                Dir::Asks,
                &q.id,
                &rec(
                    &Envelope::sign(&q, &a),
                    "waiting",
                    "2026-09-01T00:00:00Z",
                    Value::Null,
                ),
            )
            .unwrap();
        let open = open_asks(&spool).unwrap();
        let ans = Payload::answer(&q, "yes", "fake", 0, false);
        let good = Envelope::sign(&ans, &b);
        std::fs::create_dir_all(spool.path(Dir::Done, &q.id).join("child")).unwrap();
        let err = ingest_envelope(&a, &contact, &no_iroh(), &spool, &open, &good)
            .unwrap_err()
            .to_string();
        assert!(err.contains("moving"), "{err}");
        assert!(spool.get(Dir::Inbox, &ans.id).unwrap().is_some());
        assert!(spool.get(Dir::Asks, &q.id).unwrap().is_some(), "still open");
        // Inbox blocked instead: nothing moved, error names the inbox write.
        std::fs::remove_dir_all(spool.path(Dir::Done, &q.id)).unwrap();
        std::fs::remove_file(spool.path(Dir::Inbox, &ans.id)).unwrap();
        std::fs::create_dir_all(spool.path(Dir::Inbox, &ans.id).join("child")).unwrap();
        let err = ingest_envelope(&a, &contact, &no_iroh(), &spool, &open, &good)
            .unwrap_err()
            .to_string();
        assert!(err.contains("renaming to"), "{err}");
        assert!(spool.get(Dir::Asks, &q.id).unwrap().is_some(), "still open");
        assert!(spool.cache_get("h").unwrap().is_none());
    }
}
