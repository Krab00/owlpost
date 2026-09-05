//! OWL-006: daemon HTTP API over pinned mTLS on 127.0.0.1:0 (AC1..AC9).

mod common;

use std::time::{Duration, Instant};

use common::*;
use owlpost::contacts::Mode;
use owlpost::envelope::{
    self, Body, Envelope, Kind, Payload, REPLAY_WINDOW_SECS, question_hash, unix_to_rfc3339,
};
use owlpost::identity::{self, Identity};
use owlpost::spool::{Dir, Record};
use serde_json::{Value, json};

const CARD_PATHS: [&str; 2] = ["/.well-known/agent-card.json", "/.well-known/agent.json"];

fn inbox_ids(d: &TestDaemon) -> Vec<String> {
    d.spool()
        .list(Dir::Inbox, |_| true)
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

fn sig_over(signer: &Identity, raw: &[u8]) -> String {
    identity::sig_string(&signer.sign(raw))
}

fn record(raw: &str, sig: &str, state: &str) -> Record {
    Record {
        raw: raw.into(),
        sig: sig.into(),
        state: state.into(),
        seen: false,
        received_at: envelope::rfc3339_now(),
        draft: None,
        meta: Value::Null,
    }
}

async fn card_at(client: &reqwest::Client, d: &TestDaemon, path: &str) -> Value {
    let resp = client.get(d.url(path)).send().await.unwrap();
    assert_eq!(resp.status(), 200, "{path}");
    resp.json().await.unwrap()
}

// AC1
/// The shared fixture must never reach the OS notifier (`tests/notify.rs` opts in on its own).
#[tokio::test]
async fn fixture_daemon_has_notifications_off() {
    let d = spawn_daemon(1, true, &[]).await;
    assert!(!d.running.state.config.notify, "fixture must not notify");
    assert!(
        !owlpost::config::Config::load(d.home()).unwrap().notify,
        "saved fixture config must not notify"
    );
    d.running.shutdown();
}

#[tokio::test]
async fn card_is_served_unpinned_and_pinned() {
    let (a, c) = (id(1), id(3));
    let b = spawn_daemon(2, true, &[Peer::new(&a, "Ana", None)]).await;
    let unpinned = client(None, &b.id);
    let pinned = client(Some(&a), &b.id);
    for path in CARD_PATHS {
        for (who, cl) in [("unpinned", &unpinned), ("pinned", &pinned)] {
            let card = card_at(cl, &b, path).await;
            let owl = card.get("owlpost").expect("owlpost block");
            assert_eq!(owl["fingerprint"], b.fp(), "{who} {path}");
            assert_eq!(
                owl["pubkey"],
                identity::pubkey_string(&b.id.verifying_key())
            );
            assert_eq!(owl["protocol"], 1);
            assert_eq!(owl["responds"], true);
            assert_eq!(owl["harness"], "claude");
            assert_eq!(card["capabilities"]["streaming"], false);
            assert_eq!(card["capabilities"]["pushNotifications"], false);
            assert_eq!(card["skills"], json!([]));
            assert_eq!(card["name"], "Bea");
            assert_eq!(card["url"], format!("https://{}/", b.addr));
            assert_eq!(card["version"], env!("CARGO_PKG_VERSION"));
            assert!(card["protocolVersion"].is_string());
            assert!(card["description"].is_string());
        }
    }
    // Card is the only thing an unpinned client gets: every /v1 route is 401.
    let resp = unpinned.get(b.url("/v1/outbox")).send().await.unwrap();
    assert_error(resp, 401, "client certificate required").await;
    let resp = post_envelope(&unpinned, &b, &signed(&a, &b.id, "why?")).await;
    assert_error(resp, 401, "client certificate required").await;
    let resp = unpinned
        .post(b.url("/v1/outbox/some-id/ack"))
        .send()
        .await
        .unwrap();
    assert_error(resp, 401, "client certificate required").await;
    assert!(inbox_ids(&b).is_empty());
    // A certificate that is not in the book fails the handshake outright.
    let unknown = client(Some(&c), &b.id);
    for path in CARD_PATHS {
        assert!(unknown.get(b.url(path)).send().await.is_err(), "{path}");
    }
    // Unknown route and wrong method are JSON 4xx too.
    let resp = pinned.get(b.url("/v1/nope")).send().await.unwrap();
    assert_error(resp, 404, "not found").await;
    let resp = unpinned.get(b.url("/nope")).send().await.unwrap();
    assert_error(resp, 404, "not found").await;
    let resp = pinned.get(b.url("/v1/questions")).send().await.unwrap();
    assert_error(resp, 405, "method not allowed").await;
    let resp = unpinned.post(b.url(CARD_PATHS[0])).send().await.unwrap();
    assert_error(resp, 405, "method not allowed").await;
    // Oversized body is 413 JSON, not a dropped connection.
    let big = vec![b' '; owlpost::server::MAX_BODY_BYTES + 1024];
    let resp = post_raw(&pinned, &b, big, Some("ed25519:AAAA")).await;
    assert_error(resp, 413, "body too large").await;
    b.running.shutdown();

    // The card echoes the configured harness, not a constant.
    let b = spawn_daemon_with(4, &[Peer::new(&a, "Ana", None)], |cfg| {
        cfg.responder.harness = "codex".into();
        cfg.name = "Cody".into();
        cfg.endpoints = vec!["cody.example.org:7411".into()];
    })
    .await;
    for path in CARD_PATHS {
        let card = card_at(&client(None, &b.id), &b, path).await;
        assert_eq!(card["owlpost"]["harness"], "codex");
        assert_eq!(card["name"], "Cody");
        assert_eq!(card["url"], "https://cody.example.org:7411/");
        assert_eq!(card["owlpost"]["fingerprint"], b.fp());
    }
    b.running.shutdown();

    // A daemon with no contacts at all still serves the card to unpinned clients.
    let b = spawn_daemon(5, true, &[]).await;
    let card = card_at(&client(None, &b.id), &b, CARD_PATHS[1]).await;
    assert_eq!(card["owlpost"]["fingerprint"], b.fp());
    assert!(
        client(Some(&a), &b.id)
            .get(b.url(CARD_PATHS[0]))
            .send()
            .await
            .is_err()
    );
    b.running.shutdown();
}

// AC2
#[tokio::test]
async fn question_is_accepted_and_spooled() {
    let (a, c) = (id(1), id(3));
    let b = spawn_daemon(
        2,
        true,
        &[
            Peer::new(&a, "Ana", Some(policy(Mode::Manual, None))),
            Peer::new(&c, "Cat", Some(policy(Mode::Auto, None))),
        ],
    )
    .await;
    let env = signed(&a, &b.id, "Why is the refresh token rotated on every read?");
    let payload: Payload = serde_json::from_str(&env.raw).unwrap();
    let resp = post_envelope(&client(Some(&a), &b.id), &b, &env).await;
    assert_eq!(resp.status(), 202);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body, json!({ "status": "accepted", "id": payload.id }));

    let rec = b
        .spool()
        .get(Dir::Inbox, &payload.id)
        .unwrap()
        .expect("inbox record");
    assert_eq!(rec.state, "pending");
    assert_eq!(rec.raw, env.raw, "raw bytes stored exactly as signed");
    assert_eq!(rec.sig, env.sig);
    assert!(!rec.seen);
    assert!(rec.draft.is_none());
    assert_eq!(rec.meta["peer"], fp(&a));
    assert_eq!(
        rec.meta["hash"],
        question_hash(
            PROJECT,
            Some(PATH),
            "Why is the refresh token rotated on every read?"
        )
    );
    assert!(envelope::is_fresh(
        &rec.received_at,
        envelope::now_unix(),
        5
    ));
    assert_eq!(inbox_ids(&b), vec![payload.id.clone()]);
    let seen = std::fs::read_to_string(b.home().join("seen-ids.txt")).unwrap();
    assert!(seen.starts_with(&format!("{} ", payload.id)), "{seen}");
    for d in [Dir::Outbox, Dir::Asks, Dir::Done, Dir::Cache] {
        assert!(b.spool().list(d, |_| true).unwrap().is_empty());
    }

    // `auto` is also stored as pending (the scheduler picks it up later).
    let env_c = signed(&c, &b.id, "auto?");
    let resp = post_envelope(&client(Some(&c), &b.id), &b, &env_c).await;
    assert_eq!(resp.status(), 202);
    let id_c = serde_json::from_str::<Payload>(&env_c.raw).unwrap().id;
    let rec = b.spool().get(Dir::Inbox, &id_c).unwrap().unwrap();
    assert_eq!(rec.state, "pending");
    assert_eq!(rec.meta["peer"], fp(&c));
    assert_eq!(inbox_ids(&b).len(), 2);
    b.running.shutdown();
}

// AC3
#[tokio::test]
async fn no_policy_means_consent() {
    let (a, c) = (id(1), id(3));
    let b = spawn_daemon(
        2,
        true,
        &[
            Peer::new(&a, "Ana", None),
            Peer::new(&c, "Cat", Some(policy(Mode::Manual, None))),
        ],
    )
    .await;
    let env = signed(&a, &b.id, "may I?");
    let resp = post_envelope(&client(Some(&a), &b.id), &b, &env).await;
    assert_eq!(resp.status(), 202);
    let id_a = serde_json::from_str::<Payload>(&env.raw).unwrap().id;
    let rec = b.spool().get(Dir::Inbox, &id_a).unwrap().unwrap();
    assert_eq!(rec.state, "consent");
    assert_eq!(rec.raw, env.raw);
    assert_eq!(rec.meta["peer"], fp(&a));
    // Same daemon, peer with a policy: pending, not consent.
    let env_c = signed(&c, &b.id, "may I?");
    let resp = post_envelope(&client(Some(&c), &b.id), &b, &env_c).await;
    assert_eq!(resp.status(), 202);
    let id_c = serde_json::from_str::<Payload>(&env_c.raw).unwrap().id;
    assert_eq!(
        b.spool().get(Dir::Inbox, &id_c).unwrap().unwrap().state,
        "pending"
    );
    // Policy files are re-read per request: `owl allow` while running takes effect.
    write_contact(
        b.home(),
        &Peer::new(&a, "Ana", Some(policy(Mode::Manual, None))),
    );
    let env2 = signed(&a, &b.id, "and now?");
    let resp = post_envelope(&client(Some(&a), &b.id), &b, &env2).await;
    assert_eq!(resp.status(), 202);
    let id2 = serde_json::from_str::<Payload>(&env2.raw).unwrap().id;
    assert_eq!(
        b.spool().get(Dir::Inbox, &id2).unwrap().unwrap().state,
        "pending"
    );
    // Contact removed while running: pinned at the TLS layer, but no longer known.
    std::fs::remove_file(b.home().join("contacts").join(format!("{}.json", fp(&a)))).unwrap();
    let resp = post_envelope(&client(Some(&a), &b.id), &b, &signed(&a, &b.id, "gone?")).await;
    assert_error(resp, 403, "unknown peer").await;
    assert_eq!(inbox_ids(&b).len(), 3);
    b.running.shutdown();
}

// AC4
#[tokio::test]
async fn never_and_disabled_return_unavailable() {
    let (a, c) = (id(1), id(3));
    // Responder enabled, peer denied.
    let b = spawn_daemon(
        2,
        true,
        &[
            Peer::new(&a, "Ana", Some(policy(Mode::Never, None))),
            Peer::new(&c, "Cat", Some(policy(Mode::Manual, None))),
        ],
    )
    .await;
    let unpinned = client(None, &b.id);
    assert_eq!(
        card_at(&unpinned, &b, CARD_PATHS[0]).await["owlpost"]["responds"],
        true
    );
    let env = signed(&a, &b.id, "why?");
    let resp = post_envelope(&client(Some(&a), &b.id), &b, &env).await;
    assert_error(resp, 403, "unavailable").await;
    assert!(inbox_ids(&b).is_empty(), "never must not spool");
    assert!(
        !b.home().join("seen-ids.txt").exists(),
        "never must not burn the id"
    );
    // A cached answer does not bypass `never`.
    let q: Payload = serde_json::from_str(&env.raw).unwrap();
    let ans = Envelope::sign(&Payload::answer(&q, "Because.", "fake", 0, true), &b.id);
    b.spool()
        .cache_put(
            &question_hash(PROJECT, Some(PATH), "why?"),
            &record(&ans.raw, &ans.sig, "cached"),
        )
        .unwrap();
    let resp = post_envelope(&client(Some(&a), &b.id), &b, &env).await;
    assert_error(resp, 403, "unavailable").await;
    // The other peer is unaffected.
    let resp = post_envelope(&client(Some(&c), &b.id), &b, &signed(&c, &b.id, "ok?")).await;
    assert_eq!(resp.status(), 202);
    assert_eq!(inbox_ids(&b).len(), 1);
    b.running.shutdown();

    // Responder disabled: every peer, with or without a policy, and even a cache hit.
    let b = spawn_daemon(
        4,
        false,
        &[
            Peer::new(&a, "Ana", Some(policy(Mode::Manual, None))),
            Peer::new(&c, "Cat", None),
        ],
    )
    .await;
    let unpinned = client(None, &b.id);
    for path in CARD_PATHS {
        assert_eq!(
            card_at(&unpinned, &b, path).await["owlpost"]["responds"],
            false
        );
    }
    let env = signed(&a, &b.id, "why?");
    let q: Payload = serde_json::from_str(&env.raw).unwrap();
    let ans = Envelope::sign(&Payload::answer(&q, "Because.", "fake", 0, true), &b.id);
    b.spool()
        .cache_put(
            &question_hash(PROJECT, Some(PATH), "why?"),
            &record(&ans.raw, &ans.sig, "cached"),
        )
        .unwrap();
    let resp = post_envelope(&client(Some(&a), &b.id), &b, &env).await;
    assert_error(resp, 403, "unavailable").await;
    let resp = post_envelope(&client(Some(&c), &b.id), &b, &signed(&c, &b.id, "why?")).await;
    assert_error(resp, 403, "unavailable").await;
    assert!(inbox_ids(&b).is_empty());
    assert!(!b.home().join("seen-ids.txt").exists());
    b.running.shutdown();
}

// AC5
#[tokio::test]
async fn bad_inputs_are_4xx_never_500() {
    let (a, c) = (id(1), id(3));
    let b = spawn_daemon(
        2,
        true,
        &[
            Peer::new(&a, "Ana", Some(policy(Mode::Manual, None))),
            Peer::new(&c, "Cat", Some(policy(Mode::Manual, None))),
        ],
    )
    .await;
    let cl = client(Some(&a), &b.id);
    let now = envelope::now_unix();
    let good = question(&a, &b.id, "why?");
    let good_raw = good.to_signed_bytes();

    // Wrong signature: C signs A's payload. Rejected before any write.
    let resp = post_raw(&cl, &b, good_raw.clone(), Some(&sig_over(&c, &good_raw))).await;
    assert_error(resp, 400, "bad signature").await;
    // Tampered body under A's real signature.
    let mut tampered = good_raw.clone();
    tampered[2] = b'V';
    let resp = post_raw(&cl, &b, tampered, Some(&sig_over(&a, &good_raw))).await;
    assert_error(resp, 400, "bad signature").await;
    assert!(inbox_ids(&b).is_empty());
    assert!(!b.home().join("seen-ids.txt").exists());
    // Same id, valid this time: not a duplicate, because the rejects never recorded it.
    let resp = post_raw(&cl, &b, good_raw.clone(), Some(&sig_over(&a, &good_raw))).await;
    assert_eq!(resp.status(), 202);
    assert_eq!(inbox_ids(&b), vec![good.id.clone()]);
    // Duplicate id.
    let resp = post_raw(&cl, &b, good_raw.clone(), Some(&sig_over(&a, &good_raw))).await;
    assert_error(resp, 409, "duplicate id").await;

    // Signed by A but `from` claims C (C is a real contact).
    let mut from_c = question(&a, &b.id, "why?");
    from_c.from = fp(&c);
    let raw = from_c.to_signed_bytes();
    let resp = post_raw(&cl, &b, raw.clone(), Some(&sig_over(&a, &raw))).await;
    assert_error(resp, 400, "from does not match the client certificate").await;
    // Addressed to someone else.
    let mut to_c = question(&a, &b.id, "why?");
    to_c.to = fp(&c);
    let raw = to_c.to_signed_bytes();
    let resp = post_raw(&cl, &b, raw.clone(), Some(&sig_over(&a, &raw))).await;
    assert_error(resp, 400, "to is not this daemon").await;

    // Replay window: past, future, boundary, unparsable.
    for (ts, err) in [
        (unix_to_rfc3339(now - REPLAY_WINDOW_SECS - 2), "stale ts"),
        (unix_to_rfc3339(now + REPLAY_WINDOW_SECS + 2), "stale ts"),
        ("2026-09-01 10:00:00".to_string(), "malformed ts"),
        ("garbage".to_string(), "malformed ts"),
    ] {
        let mut p = question(&a, &b.id, "why?");
        p.ts = ts.clone();
        let raw = p.to_signed_bytes();
        let resp = post_raw(&cl, &b, raw.clone(), Some(&sig_over(&a, &raw))).await;
        assert_error(resp, 400, err).await;
    }
    let mut edge = question(&a, &b.id, "why?");
    edge.ts = unix_to_rfc3339(now - REPLAY_WINDOW_SECS + 2);
    let raw = edge.to_signed_bytes();
    let resp = post_raw(&cl, &b, raw.clone(), Some(&sig_over(&a, &raw))).await;
    assert_eq!(resp.status(), 202, "inside the window is accepted");

    // Signature header problems.
    let raw = question(&a, &b.id, "why?").to_signed_bytes();
    let resp = post_raw(&cl, &b, raw.clone(), None).await;
    assert_error(resp, 400, "missing X-Owl-Signature header").await;
    let bare = sig_over(&a, &raw).replacen("ed25519:", "", 1);
    let resp = post_raw(&cl, &b, raw.clone(), Some(&bare)).await;
    assert_error(resp, 400, "malformed X-Owl-Signature header").await;
    let wrong_prefix = sig_over(&a, &raw).replacen("ed25519:", "rsa:", 1);
    let resp = post_raw(&cl, &b, raw.clone(), Some(&wrong_prefix)).await;
    assert_error(resp, 400, "malformed X-Owl-Signature header").await;
    let resp = post_raw(&cl, &b, raw.clone(), Some("ed25519:AAAA")).await;
    assert_error(resp, 400, "malformed X-Owl-Signature header").await;
    let resp = post_raw(&cl, &b, raw.clone(), Some("ed25519:not*base64")).await;
    assert_error(resp, 400, "malformed X-Owl-Signature header").await;

    // Body shape problems, each under a valid signature over exactly those bytes.
    let shape = |v: Value| serde_json::to_vec(&v).unwrap();
    let mut base = serde_json::to_value(question(&a, &b.id, "why?")).unwrap();
    base["ts"] = json!(unix_to_rfc3339(now));
    let with = |f: &dyn Fn(&mut Value)| {
        let mut v = base.clone();
        f(&mut v);
        v
    };
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (b"not json".to_vec(), "body is not JSON"),
        (b"".to_vec(), "body is not JSON"),
        (shape(json!([1, 2])), "payload must be a JSON object"),
        (shape(json!("text")), "payload must be a JSON object"),
        (
            shape(with(&|v| {
                v["body"].as_object_mut().unwrap().remove("question");
            })),
            "missing body.question",
        ),
        (
            shape(with(&|v| v["body"]["question"] = json!(""))),
            "body.question is empty",
        ),
        (
            shape(with(&|v| v["body"]["question"] = json!(["why?"]))),
            "missing body.question",
        ),
        (
            shape(with(&|v| v["body"] = json!("why?"))),
            "body must be an object",
        ),
        (
            shape(with(&|v| {
                v["body"].as_object_mut().unwrap().remove("project");
            })),
            "missing body.project",
        ),
        // OWL-018: a missing path is a valid repo-level question (asserted below); a
        // non-string path is still rejected.
        (
            shape(with(&|v| v["body"]["path"] = json!(["f"]))),
            "bad schema: data did not match any variant of untagged enum Body",
        ),
        (
            shape(with(&|v| v["type"] = json!("answer"))),
            "type must be question",
        ),
        (shape(with(&|v| v["v"] = json!(2))), "unsupported v"),
        (
            shape(with(&|v| {
                v.as_object_mut().unwrap().remove("id");
            })),
            "missing id",
        ),
        (shape(with(&|v| v["id"] = json!("../etc"))), "malformed id"),
        (shape(with(&|v| v["from"] = json!(null))), "missing from"),
        (
            shape(with(&|v| {
                v.as_object_mut().unwrap().remove("ts");
            })),
            "missing ts",
        ),
        (
            shape(with(&|v| v["in_reply_to"] = json!(7))),
            "bad schema: invalid type: integer `7`, expected a string",
        ),
    ];
    for (body, err) in cases {
        let resp = post_raw(&cl, &b, body.clone(), Some(&sig_over(&a, &body))).await;
        assert_error(resp, 400, err).await;
    }
    // OWL-018: no `body.path` at all is a valid repo-level question and is spooled as such.
    let mut no_path = base.clone();
    no_path["id"] = json!("0191c7a0-0000-7000-8000-00000000d0d0");
    no_path["body"].as_object_mut().unwrap().remove("path");
    let raw = shape(no_path);
    let resp = post_raw(&cl, &b, raw.clone(), Some(&sig_over(&a, &raw))).await;
    assert_eq!(resp.status(), 202, "a question without a path is accepted");
    let rec = b
        .spool()
        .get(Dir::Inbox, "0191c7a0-0000-7000-8000-00000000d0d0")
        .unwrap()
        .expect("spooled");
    let spooled: owlpost::envelope::Payload = serde_json::from_str(&rec.raw).unwrap();
    assert!(
        matches!(spooled.body, Body::Question { path: None, .. }),
        "{spooled:?}"
    );
    assert_eq!(
        rec.meta["hash"],
        question_hash(PROJECT, None, "why?"),
        "spooled under the None-path hash"
    );
    assert_ne!(rec.meta["hash"], question_hash(PROJECT, Some(PATH), "why?"));
    assert_ne!(rec.meta["hash"], question_hash(PROJECT, Some("-"), "why?"));
    // Only the three accepted questions ever reached the spool.
    assert_eq!(inbox_ids(&b).len(), 3);
    // Bad ack ids are 404, not 500.
    for id in ["", "nope", "..", "a b"] {
        let resp = cl
            .post(b.url(&format!("/v1/outbox/{id}/ack")))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404, "ack {id:?}");
    }
    // The same id from another peer is still a duplicate.
    let mut from_c = question(&c, &b.id, "why?");
    from_c.id = good.id.clone();
    let raw = from_c.to_signed_bytes();
    let resp = post_raw(
        &client(Some(&c), &b.id),
        &b,
        raw.clone(),
        Some(&sig_over(&c, &raw)),
    )
    .await;
    assert_error(resp, 409, "duplicate id").await;

    // Seen ids survive a daemon restart on the same home (§6: seen-ids.txt).
    b.running.shutdown();
    let TestDaemon { dir, id: b_id, .. } = b;
    let b = respawn(dir, b_id).await;
    let cl = client(Some(&a), &b.id);
    let resp = post_raw(&cl, &b, good_raw.clone(), Some(&sig_over(&a, &good_raw))).await;
    assert_error(resp, 409, "duplicate id").await;
    let fresh = signed(&a, &b.id, "after restart?");
    let resp = post_envelope(&cl, &b, &fresh).await;
    assert_eq!(resp.status(), 202, "new ids still accepted after restart");
    assert_eq!(inbox_ids(&b).len(), 4);
    b.running.shutdown();
}

// AC6
#[tokio::test]
async fn rate_limit_trips() {
    let (a, c) = (id(1), id(3));
    let b = spawn_daemon(
        2,
        true,
        &[
            Peer::new(&a, "Ana", Some(policy(Mode::Manual, Some(3)))),
            Peer::new(&c, "Cat", Some(policy(Mode::Manual, Some(3)))),
        ],
    )
    .await;
    let cl = client(Some(&a), &b.id);
    let start = Instant::now();
    for i in 0..3 {
        let resp = post_envelope(&cl, &b, &signed(&a, &b.id, &format!("q{i}"))).await;
        assert_eq!(resp.status(), 202, "question {i}");
    }
    let fourth = signed(&a, &b.id, "q3");
    let resp = post_envelope(&cl, &b, &fourth).await;
    assert!(start.elapsed() < Duration::from_secs(60));
    let headers = assert_error(resp, 429, "rate limited").await;
    let retry: u64 = headers["retry-after"].to_str().unwrap().parse().unwrap();
    assert_eq!(
        retry, 1200,
        "3/h bucket: one token every 1200 s, rounded up"
    );
    assert_eq!(inbox_ids(&b).len(), 3, "the 4th question is not spooled");
    // A rate-limited id is not burned: the same envelope is still 429, never 409.
    let resp = post_envelope(&cl, &b, &fourth).await;
    assert_error(resp, 429, "rate limited").await;
    // Buckets are per peer.
    let resp = post_envelope(&client(Some(&c), &b.id), &b, &signed(&c, &b.id, "c0")).await;
    assert_eq!(resp.status(), 202);
    assert_eq!(inbox_ids(&b).len(), 4);
    b.running.shutdown();

    // Rate limit sits before policy (architecture §3.2): a denied peer burns tokens too.
    let d = id(4);
    let b = spawn_daemon(
        5,
        true,
        &[Peer::new(&d, "Dee", Some(policy(Mode::Never, Some(3))))],
    )
    .await;
    let cl = client(Some(&d), &b.id);
    for i in 0..3 {
        let resp = post_envelope(&cl, &b, &signed(&d, &b.id, &format!("n{i}"))).await;
        assert_error(resp, 403, "unavailable").await;
    }
    let resp = post_envelope(&cl, &b, &signed(&d, &b.id, "n3")).await;
    let headers = assert_error(resp, 429, "rate limited").await;
    assert_eq!(headers["retry-after"], "1200");
    assert!(inbox_ids(&b).is_empty());
    b.running.shutdown();

    // No per-contact rate: the global `rate_limit_per_peer_per_hour` applies.
    let b = spawn_daemon_with(
        6,
        &[
            Peer::new(&a, "Ana", Some(policy(Mode::Manual, None))),
            Peer::new(&c, "Cat", None),
        ],
        |cfg| cfg.rate_limit_per_peer_per_hour = 2,
    )
    .await;
    for (who, name) in [(&a, "a"), (&c, "c")] {
        let cl = client(Some(who), &b.id);
        for i in 0..2 {
            let resp = post_envelope(&cl, &b, &signed(who, &b.id, &format!("{name}{i}"))).await;
            assert_eq!(resp.status(), 202, "{name}{i}");
        }
        let resp = post_envelope(&cl, &b, &signed(who, &b.id, &format!("{name}2"))).await;
        let headers = assert_error(resp, 429, "rate limited").await;
        assert_eq!(
            headers["retry-after"], "1800",
            "2/h: one token every 1800 s"
        );
    }
    assert_eq!(inbox_ids(&b).len(), 4);
    b.running.shutdown();
}

// AC7
#[tokio::test]
async fn cache_hit_returns_answer() {
    let a = id(1);
    let b = spawn_daemon(
        2,
        true,
        &[Peer::new(&a, "Ana", Some(policy(Mode::Manual, None)))],
    )
    .await;
    let cl = client(Some(&a), &b.id);
    let text = "Why is the refresh token rotated on every read?";
    let earlier = question(&a, &b.id, text);
    let answer = Payload::answer(
        &earlier,
        "Because the session store is append-only.",
        "fake",
        0,
        false,
    );
    let ans_env = Envelope::sign(&answer, &b.id);
    b.spool()
        .cache_put(
            &question_hash(PROJECT, Some(PATH), text),
            &record(&ans_env.raw, &ans_env.sig, "cached"),
        )
        .unwrap();

    // Same question, normalised differently: whitespace and case do not matter.
    let env = signed(
        &a,
        &b.id,
        "  why IS the refresh token\trotated on every read?  ",
    );
    let resp = post_envelope(&cl, &b, &env).await;
    assert_eq!(resp.status(), 200);
    let sig = resp.headers()["x-owl-signature"]
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(sig, ans_env.sig);
    assert_eq!(
        resp.headers()["content-type"].to_str().unwrap(),
        "application/json"
    );
    let body = resp.text().await.unwrap();
    assert_eq!(
        body, ans_env.raw,
        "body is the payload exactly as B signed it"
    );
    let got = Envelope {
        raw: body,
        sig: sig.clone(),
    }
    .verify(&b.id.verifying_key())
    .expect("answer must verify with B's key");
    assert_eq!(got, answer);
    assert_eq!(got.kind, Kind::Answer);
    assert_eq!(got.from, b.fp());
    assert_eq!(got.to, fp(&a));
    assert_eq!(got.in_reply_to.as_deref(), Some(earlier.id.as_str()));
    assert!(matches!(&got.body, Body::Answer { answer, .. } if answer.starts_with("Because")));
    assert!(
        Envelope {
            raw: ans_env.raw.clone(),
            sig
        }
        .verify(&a.verifying_key())
        .is_err(),
        "not A's signature"
    );
    // A cache hit is not spooled, but its id still counts as seen.
    assert!(inbox_ids(&b).is_empty());
    let resp = post_envelope(&cl, &b, &env).await;
    assert_error(resp, 409, "duplicate id").await;
    // A different question misses the cache and is spooled as usual.
    let resp = post_envelope(&cl, &b, &signed(&a, &b.id, "something else?")).await;
    assert_eq!(resp.status(), 202);
    assert_eq!(inbox_ids(&b).len(), 1);

    // OWL-018: a repo-level question hits the cache only under the None-path hash; the same
    // words about a file are a different key and get spooled.
    let repo_text = "How long is your README?";
    let repo_q = Payload::question(&fp(&a), &b.fp(), PROJECT, None, repo_text);
    let repo_ans = Payload::answer(&repo_q, "About 80 lines.", "fake", 0, false);
    let repo_env = Envelope::sign(&repo_ans, &b.id);
    b.spool()
        .cache_put(
            &question_hash(PROJECT, None, repo_text),
            &record(&repo_env.raw, &repo_env.sig, "cached"),
        )
        .unwrap();
    let ask = Envelope::sign(
        &Payload::question(&fp(&a), &b.fp(), PROJECT, None, repo_text),
        &a,
    );
    let resp = post_envelope(&cl, &b, &ask).await;
    assert_eq!(resp.status(), 200, "None-path hash hit");
    assert_eq!(resp.text().await.unwrap(), repo_env.raw);
    let twin = Envelope::sign(
        &Payload::question(&fp(&a), &b.fp(), PROJECT, Some(PATH), repo_text),
        &a,
    );
    let resp = post_envelope(&cl, &b, &twin).await;
    assert_eq!(resp.status(), 202, "the with-path twin is a different key");
    assert_eq!(inbox_ids(&b).len(), 2);
    b.running.shutdown();

    // Policy is checked before the cache (architecture §3.2). Same cached hash, four peers:
    // no policy → consent record, never a cached answer; never → 403; manual/auto → 200.
    let (none, never, manual, auto) = (id(3), id(4), id(5), id(6));
    let b = spawn_daemon(
        7,
        true,
        &[
            Peer::new(&none, "None", None),
            Peer::new(&never, "Never", Some(policy(Mode::Never, None))),
            Peer::new(&manual, "Manual", Some(policy(Mode::Manual, None))),
            Peer::new(&auto, "Auto", Some(policy(Mode::Auto, None))),
        ],
    )
    .await;
    let earlier = question(&manual, &b.id, text);
    let ans_env = Envelope::sign(
        &Payload::answer(&earlier, "Cached.", "fake", 0, true),
        &b.id,
    );
    b.spool()
        .cache_put(
            &question_hash(PROJECT, Some(PATH), text),
            &record(&ans_env.raw, &ans_env.sig, "cached"),
        )
        .unwrap();
    let env = signed(&none, &b.id, text);
    let resp = post_envelope(&client(Some(&none), &b.id), &b, &env).await;
    assert_eq!(resp.status(), 202, "no policy: never a cached answer");
    let id_none = serde_json::from_str::<Payload>(&env.raw).unwrap().id;
    assert_eq!(
        b.spool().get(Dir::Inbox, &id_none).unwrap().unwrap().state,
        "consent"
    );
    let resp = post_envelope(
        &client(Some(&never), &b.id),
        &b,
        &signed(&never, &b.id, text),
    )
    .await;
    assert_error(resp, 403, "unavailable").await;
    for (who, name) in [(&manual, "manual"), (&auto, "auto")] {
        let resp = post_envelope(&client(Some(who), &b.id), &b, &signed(who, &b.id, text)).await;
        assert_eq!(resp.status(), 200, "{name}: cache hit");
        assert_eq!(resp.headers()["x-owl-signature"], ans_env.sig.as_str());
        assert_eq!(resp.text().await.unwrap(), ans_env.raw);
    }
    assert_eq!(
        inbox_ids(&b),
        vec![id_none],
        "only the consent record was spooled"
    );
    b.running.shutdown();
}

// AC8
#[tokio::test]
async fn outbox_is_per_caller_and_ack_moves_to_done() {
    let (a, c) = (id(1), id(3));
    let b = spawn_daemon(
        2,
        true,
        &[Peer::new(&a, "Ana", None), Peer::new(&c, "Cat", None)],
    )
    .await;
    let spool = b.spool();
    let mk = |asker: &Identity, text: &str| {
        let q = question(asker, &b.id, text);
        let env = Envelope::sign(&Payload::answer(&q, "Answer.", "fake", 0, false), &b.id);
        let id = serde_json::from_str::<Payload>(&env.raw).unwrap().id;
        spool
            .put(Dir::Outbox, &id, &record(&env.raw, &env.sig, "unacked"))
            .unwrap();
        (id, env)
    };
    let (id_a, env_a) = mk(&a, "for A?");
    let (id_c, env_c) = mk(&c, "for C?");
    // A stray *question* record addressed to A is never listed nor ack-able: only answers are.
    let stray = Envelope::sign(&question(&b.id, &a, "stray question to A"), &b.id);
    assert_eq!(
        serde_json::from_str::<Payload>(&stray.raw).unwrap().to,
        fp(&a)
    );
    spool
        .put(
            Dir::Outbox,
            "stray",
            &record(&stray.raw, &stray.sig, "unacked"),
        )
        .unwrap();
    // A corrupt file in outbox/ is skipped, never a 500.
    std::fs::write(spool.path(Dir::Outbox, "corrupt"), b"{not json").unwrap();

    let cl_a = client(Some(&a), &b.id);
    let cl_c = client(Some(&c), &b.id);
    let list = |cl: &reqwest::Client| {
        let cl = cl.clone();
        let url = b.url("/v1/outbox");
        async move {
            let resp = cl.get(url).send().await.unwrap();
            assert_eq!(resp.status(), 200);
            resp.json::<Vec<Value>>().await.unwrap()
        }
    };
    assert_eq!(
        list(&cl_a).await,
        vec![json!({ "raw": env_a.raw, "sig": env_a.sig })]
    );
    assert_eq!(
        list(&cl_c).await,
        vec![json!({ "raw": env_c.raw, "sig": env_c.sig })]
    );

    // A blocked done/ target: the ack fails and the outbox record is NOT marked acked.
    let blocker = spool.path(Dir::Done, &id_a);
    std::fs::create_dir_all(blocker.join("child")).unwrap();
    let resp = cl_a
        .post(b.url(&format!("/v1/outbox/{id_a}/ack")))
        .send()
        .await
        .unwrap();
    assert_error(resp, 500, "storage error").await;
    assert_eq!(
        spool.get(Dir::Outbox, &id_a).unwrap().unwrap().state,
        "unacked",
        "failed move must not leave an acked record in outbox"
    );
    assert_eq!(list(&cl_a).await.len(), 1, "still listed for A");
    std::fs::remove_dir_all(&blocker).unwrap();

    // Ack by the addressee: 204, record lands in done/ as acked.
    let resp = cl_a
        .post(b.url(&format!("/v1/outbox/{id_a}/ack")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    assert!(spool.get(Dir::Outbox, &id_a).unwrap().is_none());
    let done = spool.get(Dir::Done, &id_a).unwrap().expect("in done/");
    assert_eq!(done.state, "acked");
    assert_eq!(done.raw, env_a.raw);
    assert_eq!(done.sig, env_a.sig);
    assert!(list(&cl_a).await.is_empty());
    assert_eq!(list(&cl_c).await.len(), 1, "C's answer untouched");

    // Ack of someone else's answer, of an already acked id, of an unknown id: all 404.
    for id in [id_c.as_str(), id_a.as_str(), "stray", "corrupt", "unknown"] {
        let resp = cl_a
            .post(b.url(&format!("/v1/outbox/{id}/ack")))
            .send()
            .await
            .unwrap();
        assert_error(resp, 404, "not found").await;
    }
    assert_eq!(
        spool.get(Dir::Outbox, &id_c).unwrap().unwrap().state,
        "unacked"
    );
    assert!(spool.get(Dir::Done, &id_c).unwrap().is_none());
    assert_eq!(
        spool.get(Dir::Outbox, "stray").unwrap().unwrap().state,
        "unacked"
    );
    assert!(spool.path(Dir::Outbox, "corrupt").exists());
    // C can still ack its own.
    let resp = cl_c
        .post(b.url(&format!("/v1/outbox/{id_c}/ack")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    assert_eq!(spool.get(Dir::Done, &id_c).unwrap().unwrap().state, "acked");
    b.running.shutdown();
}

struct Child(std::process::Child);

impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// AC9
#[tokio::test]
async fn daemon_foreground_writes_addr_and_serves_card() {
    let (a, b) = (id(1), id(2));
    let dir = tempfile::tempdir().unwrap();
    prepare_home(dir.path(), &b, true, &[Peer::new(&a, "Ana", None)]);
    let child = Child(
        std::process::Command::new(env!("CARGO_BIN_EXE_owl"))
            .args([
                "--home",
                dir.path().to_str().unwrap(),
                "daemon",
                "--foreground",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let addr_file = dir.path().join("daemon.addr");
    let deadline = Instant::now() + Duration::from_secs(2);
    let addr = loop {
        if let Ok(text) = std::fs::read_to_string(&addr_file)
            && let Ok(addr) = text.trim().parse::<std::net::SocketAddr>()
        {
            break addr;
        }
        assert!(
            Instant::now() < deadline,
            "daemon.addr not written within 2 s"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(addr.ip().is_loopback());
    assert_ne!(addr.port(), 0);
    assert!(!dir.path().join("daemon.addr.tmp").exists());
    let card: Value = client(None, &b)
        .get(format!("https://{addr}/.well-known/agent-card.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(card["owlpost"]["fingerprint"], fp(&b));
    assert_eq!(card["owlpost"]["protocol"], 1);
    let resp = client(Some(&a), &b)
        .get(format!("https://{addr}/v1/outbox"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "pinned route works on the real binary");
    drop(child);

    // Without an identity the command refuses and writes nothing.
    let empty = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_owl"))
        .args([
            "--home",
            empty.path().to_str().unwrap(),
            "daemon",
            "--foreground",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("owl init"));
    assert!(!empty.path().join("daemon.addr").exists());
}
