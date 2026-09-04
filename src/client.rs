//! Outbound HTTP (§7 consumer): send a question, fetch the peer's outbox, ack an answer.
//! Every call tries **iroh first** (the peer's key is its address), then the contact's
//! `endpoints` in order over pinned mTLS; a transport failure (dial timeout, connect, TLS)
//! moves on to the next transport, and only when every one failed is the peer reported
//! offline. A contact with no `endpoints` is reachable over iroh alone.
//!
//! Blocking: CLI paths are synchronous (§1). A daemon task must wrap these in
//! `tokio::task::spawn_blocking`.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use axum::http::HeaderMap;
use reqwest::blocking::Client;
use reqwest::header::RETRY_AFTER;
use serde_json::Value;

use crate::contacts::Contact;
use crate::envelope::{Envelope, Kind, Payload};
use crate::identity::{self, Identity};
use crate::server::{AppState, SIGNATURE_HEADER};
use crate::tls;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// Whole-request ceiling; a `200`/`202` comes straight from the spool or cache.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// How a call reaches a peer's iroh endpoint.
#[derive(Debug, Clone)]
pub enum Iroh {
    /// Inside the daemon: dial from its own endpoint (peer address = key + `relay_urls`).
    Direct {
        endpoint: iroh::Endpoint,
        relay_urls: Option<Vec<String>>,
    },
    /// From the CLI: the local daemon at `addr` replays the request (`/v1/local/…`).
    ViaDaemon(SocketAddr),
    /// No iroh path (no running daemon); only `endpoints` are tried. Carries the reason.
    Unavailable(String),
}

impl Iroh {
    /// For the CLI: the daemon named by `<home>/daemon.addr`, else unavailable.
    pub fn from_home(home: &Path) -> Iroh {
        match crate::daemon::local_addr(home) {
            Some(addr) => Iroh::ViaDaemon(addr),
            None => Iroh::Unavailable(format!(
                "no local daemon ({} missing)",
                crate::daemon::ADDR_FILE
            )),
        }
    }

    /// For the daemon's own tasks: its endpoint.
    pub fn for_daemon(state: &AppState) -> Iroh {
        match state.iroh.get() {
            Some(endpoint) => Iroh::Direct {
                endpoint: endpoint.clone(),
                relay_urls: state.config.relay_urls.clone(),
            },
            None => Iroh::Unavailable("no endpoint".into()),
        }
    }
}

/// A peer's reply, whichever transport carried it.
#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub signature: Option<String>,
    pub retry_after: Option<u64>,
    pub body: Vec<u8>,
}

impl Reply {
    fn from_headers(status: u16, headers: &HeaderMap, body: Vec<u8>) -> Reply {
        Reply {
            status,
            signature: headers
                .get(SIGNATURE_HEADER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string),
            retry_after: headers
                .get(RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse().ok()),
            body,
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// A `200` answer whose signature checked out against the contact's key.
#[derive(Debug)]
pub struct VerifiedAnswer {
    pub payload: Payload,
    pub envelope: Envelope,
}

/// What `POST /v1/questions` came back with, already verified where it carries a payload.
#[derive(Debug)]
pub enum SendOutcome {
    /// `200`: the responder's cached answer (boxed: far larger than the other variants).
    Answer(Box<VerifiedAnswer>),
    /// `202`: queued on the peer under `id` (always the question id).
    Accepted { id: String },
    /// `403`: policy `never` or responder disabled.
    Unavailable,
    /// `429`: `Retry-After` seconds when the peer sent the header.
    RateLimited { retry_after_secs: Option<u64> },
    /// Every endpoint failed at the transport level; one `endpoint: reason` per attempt.
    Offline { errors: Vec<String> },
}

/// Pinned blocking client presenting `identity`'s certificate, trusting only `contact`'s key.
pub fn http_client(identity: &Identity, contact: &Contact) -> anyhow::Result<Client> {
    let key = identity::parse_pubkey(&contact.pubkey)
        .with_context(|| format!("contact {} has a bad pubkey", contact.name))?;
    let cfg = tls::client_config(Some(identity), Some(*key.as_bytes()))?;
    Client::builder()
        .use_preconfigured_tls(Arc::unwrap_or_clone(cfg))
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .context("building HTTP client")
}

/// `https://<host:port><path>`; tolerates an endpoint written with a scheme or trailing `/`.
pub fn endpoint_url(endpoint: &str, path: &str) -> String {
    let host = endpoint
        .trim()
        .trim_start_matches("https://")
        .trim_end_matches('/');
    format!("https://{host}{path}")
}

/// One request: iroh first, then every endpoint in order. `path` starts with `/`. The
/// first transport that yields a reply wins; `Err` lists one `transport: reason` per attempt.
// ponytail: every `send()` error counts as "this endpoint is down" (refused, TLS pin mismatch,
// timeout alike) — split by `reqwest::Error` kind if a pin mismatch should stop the fallback.
fn reach(
    identity: &Identity,
    contact: &Contact,
    iroh: &Iroh,
    method: reqwest::Method,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> anyhow::Result<Result<Reply, Vec<String>>> {
    let client = http_client(identity, contact)?;
    let mut errors = Vec::new();
    match via_iroh(
        identity,
        contact,
        iroh,
        &method,
        path,
        headers,
        body.clone(),
    ) {
        Ok(reply) => return Ok(Ok(reply)),
        Err(e) => errors.push(format!("iroh: {e:#}")),
    }
    for endpoint in &contact.endpoints {
        let mut req = client
            .request(method.clone(), endpoint_url(endpoint, path))
            .body(body.clone());
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        match req.send() {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let hdrs = resp.headers().clone();
                let bytes = resp.bytes().context("reading response body")?.to_vec();
                return Ok(Ok(Reply::from_headers(status, &hdrs, bytes)));
            }
            Err(e) => errors.push(format!("{endpoint}: {}", root_cause(&e))),
        }
    }
    Ok(Err(errors))
}

/// The iroh leg of `reach`: a direct dial from the daemon, or the local daemon's forward
/// route. `Err` = the peer was not reached over iroh (the caller moves on to `endpoints`).
fn via_iroh(
    identity: &Identity,
    contact: &Contact,
    iroh: &Iroh,
    method: &reqwest::Method,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> anyhow::Result<Reply> {
    match iroh {
        Iroh::Unavailable(reason) => bail!("{reason}"),
        Iroh::Direct {
            endpoint,
            relay_urls,
        } => {
            let addr = crate::iroh::peer_addr(&contact.pubkey, relay_urls.as_deref())?;
            let mut hdrs = HeaderMap::new();
            for (k, v) in headers {
                hdrs.insert(
                    axum::http::HeaderName::from_bytes(k.as_bytes())?,
                    axum::http::HeaderValue::from_str(v)?,
                );
            }
            let method = axum::http::Method::from_bytes(method.as_str().as_bytes())?;
            let reply = tokio::runtime::Handle::current().block_on(crate::iroh::request(
                endpoint,
                addr,
                method,
                path,
                hdrs,
                body.into(),
            ))?;
            Ok(Reply::from_headers(
                reply.status,
                &reply.headers,
                reply.body.to_vec(),
            ))
        }
        Iroh::ViaDaemon(addr) => {
            let cfg =
                tls::client_config(Some(identity), Some(*identity.verifying_key().as_bytes()))?;
            let client = Client::builder()
                .use_preconfigured_tls(Arc::unwrap_or_clone(cfg))
                .connect_timeout(CONNECT_TIMEOUT)
                .timeout(crate::iroh::DIAL_TIMEOUT + REQUEST_TIMEOUT + Duration::from_secs(5))
                .build()
                .context("building HTTP client")?;
            let url = format!(
                "https://{addr}/v1/local/{}/{}",
                contact.fingerprint,
                path.trim_start_matches('/')
            );
            let mut req = client.request(method.clone(), url).body(body);
            for (k, v) in headers {
                req = req.header(*k, *v);
            }
            let resp = req
                .send()
                .map_err(|e| anyhow::anyhow!("local daemon {addr}: {}", root_cause(&e)))?;
            let status = resp.status().as_u16();
            let hdrs = resp.headers().clone();
            let bytes = resp.bytes().context("reading response body")?.to_vec();
            let reply = Reply::from_headers(status, &hdrs, bytes);
            if status == 502 {
                // The daemon did not reach the peer; its `{"error": "iroh: …"}` names why.
                let text = reply.text();
                let detail = serde_json::from_str::<Value>(&text)
                    .ok()
                    .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_string))
                    .unwrap_or(text);
                bail!("{}", detail.strip_prefix("iroh: ").unwrap_or(&detail));
            }
            Ok(reply)
        }
    }
}

/// Innermost error message (reqwest wraps "error sending request" around the real cause).
fn root_cause(e: &reqwest::Error) -> String {
    let mut cur: &dyn std::error::Error = e;
    while let Some(next) = cur.source() {
        cur = next;
    }
    cur.to_string()
}

/// `{"error": …}` from a JSON body, else the body text, else the status alone.
fn error_message(status: u16, body: &str) -> String {
    let status = reqwest::StatusCode::from_u16(status)
        .map(|s| s.to_string())
        .unwrap_or_else(|_| status.to_string());
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| body.trim().to_string());
    if detail.is_empty() {
        format!("peer returned {status}")
    } else {
        format!("peer returned {status}: {detail}")
    }
}

/// `POST /v1/questions` with the signed question. `envelope` must be a `question` payload;
/// its id is what a `202` and any `200` answer are checked against.
pub fn send_question(
    identity: &Identity,
    contact: &Contact,
    iroh: &Iroh,
    envelope: &Envelope,
) -> anyhow::Result<SendOutcome> {
    let question: Payload =
        serde_json::from_str(&envelope.raw).context("envelope is not a payload")?;
    let resp = match reach(
        identity,
        contact,
        iroh,
        reqwest::Method::POST,
        "/v1/questions",
        &[
            ("content-type", "application/json"),
            (SIGNATURE_HEADER, &envelope.sig),
        ],
        envelope.raw.clone().into_bytes(),
    )? {
        Ok(resp) => resp,
        Err(errors) => return Ok(SendOutcome::Offline { errors }),
    };
    match resp.status {
        200 => {
            let sig = resp
                .signature
                .clone()
                .context("200 answer without X-Owl-Signature header")?;
            let raw = resp.text();
            let envelope = Envelope { raw, sig };
            // The responder cache is shared across peers (§7): a 200 carries the answer to
            // the first equivalent question, so `in_reply_to` is not ours to check.
            let payload = verify_answer(contact, &envelope, None)?;
            Ok(SendOutcome::Answer(Box::new(VerifiedAnswer {
                payload,
                envelope,
            })))
        }
        202 => {
            let body: Value = serde_json::from_slice(&resp.body).context("202 body is not JSON")?;
            let id = body
                .get("id")
                .and_then(Value::as_str)
                .context("202 body has no id")?;
            if id != question.id {
                bail!(
                    "peer accepted id {id} but the question id is {}",
                    question.id
                );
            }
            Ok(SendOutcome::Accepted { id: id.to_string() })
        }
        403 => Ok(SendOutcome::Unavailable),
        429 => Ok(SendOutcome::RateLimited {
            retry_after_secs: resp.retry_after,
        }),
        status => bail!("{}", error_message(status, &resp.text())),
    }
}

/// Signature against the contact's key, then shape: kind `answer`, and when `question_id`
/// is given, `in_reply_to` must be exactly that id.
pub fn verify_answer(
    contact: &Contact,
    envelope: &Envelope,
    question_id: Option<&str>,
) -> anyhow::Result<Payload> {
    let key = identity::parse_pubkey(&contact.pubkey)
        .with_context(|| format!("contact {} has a bad pubkey", contact.name))?;
    let payload = envelope
        .verify(&key)
        .with_context(|| format!("answer from {} does not verify", contact.name))?;
    if payload.kind != Kind::Answer {
        bail!(
            "peer replied with a {:?} payload, not an answer",
            payload.kind
        );
    }
    if let Some(qid) = question_id
        && payload.in_reply_to.as_deref() != Some(qid)
    {
        bail!(
            "answer {} replies to {:?}, not to question {qid}",
            payload.id,
            payload.in_reply_to
        );
    }
    Ok(payload)
}

/// `GET /v1/outbox`: every `{raw, sig}` the peer holds for us, unverified. Offline → error.
pub fn fetch_outbox(
    identity: &Identity,
    contact: &Contact,
    iroh: &Iroh,
) -> anyhow::Result<Vec<Envelope>> {
    let resp = reach(
        identity,
        contact,
        iroh,
        reqwest::Method::GET,
        "/v1/outbox",
        &[],
        Vec::new(),
    )?
    .map_err(|errors| anyhow::anyhow!("offline: {}", errors.join("; ")))?;
    if resp.status != 200 {
        bail!("{}", error_message(resp.status, &resp.text()));
    }
    let items: Vec<Envelope> =
        serde_json::from_slice(&resp.body).context("outbox body is not [{raw, sig}]")?;
    Ok(items)
}

/// `POST /v1/outbox/{id}/ack`: `204` → ok; anything else (including offline) → error.
pub fn ack(identity: &Identity, contact: &Contact, iroh: &Iroh, id: &str) -> anyhow::Result<()> {
    let resp = reach(
        identity,
        contact,
        iroh,
        reqwest::Method::POST,
        &format!("/v1/outbox/{id}/ack"),
        &[],
        Vec::new(),
    )?
    .map_err(|errors| anyhow::anyhow!("offline: {}", errors.join("; ")))?;
    if resp.status != 204 {
        bail!("{}", error_message(resp.status, &resp.text()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::Body;

    fn contact_for(id: &Identity, endpoints: &[&str]) -> Contact {
        Contact {
            name: "Bea".into(),
            emails: vec![],
            pubkey: identity::pubkey_string(&id.verifying_key()),
            endpoints: endpoints.iter().map(|s| s.to_string()).collect(),
            source: "local".into(),
            policy: None,
            added_at: None,
            fingerprint: identity::fingerprint(&id.verifying_key()),
        }
    }

    #[test]
    fn endpoint_url_shapes() {
        assert_eq!(
            endpoint_url("127.0.0.1:7411", "/v1/outbox"),
            "https://127.0.0.1:7411/v1/outbox"
        );
        assert_eq!(
            endpoint_url("https://host:1/", "/v1/outbox"),
            "https://host:1/v1/outbox"
        );
        assert_eq!(endpoint_url(" host:1 ", "/x"), "https://host:1/x");
    }

    #[test]
    fn error_message_prefers_json_error_field() {
        let s = 409;
        assert_eq!(
            error_message(s, r#"{"error":"duplicate id"}"#),
            "peer returned 409 Conflict: duplicate id"
        );
        assert_eq!(
            error_message(s, r#"{"status":"x"}"#),
            "peer returned 409 Conflict: {\"status\":\"x\"}"
        );
        assert_eq!(
            error_message(s, "plain"),
            "peer returned 409 Conflict: plain"
        );
        assert_eq!(error_message(s, "  "), "peer returned 409 Conflict");
        assert_eq!(
            error_message(999, ""),
            "peer returned 999 <unknown status code>"
        );
    }

    #[test]
    fn verify_answer_checks_key_kind_and_reply_id() {
        let (a, b, other) = (
            Identity::from_seed([1; 32]),
            Identity::from_seed([2; 32]),
            Identity::from_seed([3; 32]),
        );
        let contact = contact_for(&b, &[]);
        let q = Payload::question("owl:a", "owl:b", "p", "f", "why?");
        let ans = Payload::answer(&q, "Because.", "fake", 0, false);
        let good = Envelope::sign(&ans, &b);
        let got = verify_answer(&contact, &good, Some(&q.id)).unwrap();
        assert_eq!(got, ans);
        assert_eq!(
            verify_answer(&contact, &good, None).unwrap(),
            ans,
            "no reply-id check when none is expected"
        );
        assert!(matches!(got.body, Body::Answer { ref answer, .. } if answer == "Because."));

        let wrong_key = Envelope::sign(&ans, &other);
        for expect in [Some(q.id.as_str()), None] {
            let err = format!(
                "{:#}",
                verify_answer(&contact, &wrong_key, expect).unwrap_err()
            );
            assert!(err.contains("does not verify"), "{expect:?}: {err}");
            let not_answer = Envelope::sign(&q, &b);
            let err = format!(
                "{:#}",
                verify_answer(&contact, &not_answer, expect).unwrap_err()
            );
            assert!(err.contains("not an answer"), "{expect:?}: {err}");
        }

        let err = format!(
            "{:#}",
            verify_answer(&contact, &good, Some("other-id")).unwrap_err()
        );
        assert!(err.contains("replies to"), "{err}");

        let bad_contact = Contact {
            pubkey: "garbage".into(),
            ..contact_for(&a, &[])
        };
        let err = format!(
            "{:#}",
            verify_answer(&bad_contact, &good, Some(&q.id)).unwrap_err()
        );
        assert!(err.contains("bad pubkey"), "{err}");
    }

    fn no_iroh() -> Iroh {
        Iroh::Unavailable("no local daemon (daemon.addr missing)".into())
    }

    /// No endpoints and no iroh: offline (not a usage error) naming the iroh reason.
    #[test]
    fn no_endpoints_without_iroh_is_offline() {
        let (a, b) = (Identity::from_seed([1; 32]), Identity::from_seed([2; 32]));
        let contact = contact_for(&b, &[]);
        let env = Envelope::sign(&Payload::question("owl:a", "owl:b", "p", "f", "why?"), &a);
        match send_question(&a, &contact, &no_iroh(), &env).unwrap() {
            SendOutcome::Offline { errors } => {
                assert_eq!(errors, ["iroh: no local daemon (daemon.addr missing)"]);
            }
            other => panic!("expected Offline, got {other:?}"),
        }
        let err = fetch_outbox(&a, &contact, &no_iroh())
            .unwrap_err()
            .to_string();
        assert_eq!(err, "offline: iroh: no local daemon (daemon.addr missing)");
        let err = ack(&a, &contact, &no_iroh(), "x").unwrap_err().to_string();
        assert_eq!(err, "offline: iroh: no local daemon (daemon.addr missing)");
    }

    #[test]
    fn closed_ports_are_offline_with_one_error_per_transport() {
        let (a, b) = (Identity::from_seed([1; 32]), Identity::from_seed([2; 32]));
        let closed = |_: ()| {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().to_string()
        };
        let (p1, p2) = (closed(()), closed(()));
        let contact = contact_for(&b, &[&p1, &p2]);
        let env = Envelope::sign(&Payload::question("owl:a", "owl:b", "p", "f", "why?"), &a);
        match send_question(&a, &contact, &no_iroh(), &env).unwrap() {
            SendOutcome::Offline { errors } => {
                assert_eq!(errors.len(), 3, "{errors:?}");
                assert_eq!(errors[0], "iroh: no local daemon (daemon.addr missing)");
                assert!(errors[1].starts_with(&p1), "{errors:?}");
                assert!(errors[2].starts_with(&p2), "{errors:?}");
            }
            other => panic!("expected Offline, got {other:?}"),
        }
        // A daemon address nobody listens on: the iroh leg fails at the local hop, the
        // endpoints are still tried afterwards.
        let dead: std::net::SocketAddr = closed(()).parse().unwrap();
        match send_question(&a, &contact, &Iroh::ViaDaemon(dead), &env).unwrap() {
            SendOutcome::Offline { errors } => {
                assert_eq!(errors.len(), 3, "{errors:?}");
                assert!(
                    errors[0].starts_with(&format!("iroh: local daemon {dead}: ")),
                    "{errors:?}"
                );
                assert!(errors[1].starts_with(&p1), "{errors:?}");
            }
            other => panic!("expected Offline, got {other:?}"),
        }
        let err = fetch_outbox(&a, &contact, &no_iroh())
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("offline: iroh: "), "{err}");
        let err = ack(&a, &contact, &no_iroh(), "x").unwrap_err().to_string();
        assert!(err.starts_with("offline: iroh: "), "{err}");
    }

    #[test]
    fn iroh_from_home_follows_daemon_addr() {
        let home = tempfile::tempdir().unwrap();
        let Iroh::Unavailable(reason) = Iroh::from_home(home.path()) else {
            panic!("no daemon.addr must be Unavailable");
        };
        assert_eq!(reason, "no local daemon (daemon.addr missing)");
        crate::daemon::write_addr_file(home.path(), "0.0.0.0:7411".parse().unwrap()).unwrap();
        let Iroh::ViaDaemon(addr) = Iroh::from_home(home.path()) else {
            panic!("daemon.addr present must be ViaDaemon");
        };
        assert_eq!(
            addr.to_string(),
            "127.0.0.1:7411",
            "wildcard mapped to loopback"
        );
        std::fs::write(home.path().join(crate::daemon::ADDR_FILE), "junk").unwrap();
        assert!(matches!(Iroh::from_home(home.path()), Iroh::Unavailable(_)));
    }
}
