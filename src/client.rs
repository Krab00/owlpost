//! Outbound HTTP (§7 consumer): send a question, fetch the peer's outbox, ack an answer.
//! Every call tries the contact's endpoints in order over pinned mTLS; a transport failure
//! (connect, TLS, timeout) moves on to the next endpoint, and only when every endpoint failed
//! is the peer reported offline.
//!
//! Blocking: CLI paths are synchronous (§1). A daemon task must wrap these in
//! `tokio::task::spawn_blocking`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use reqwest::blocking::{Client, Response};
use reqwest::header::RETRY_AFTER;
use serde_json::Value;

use crate::contacts::Contact;
use crate::envelope::{Envelope, Kind, Payload};
use crate::identity::{self, Identity};
use crate::server::SIGNATURE_HEADER;
use crate::tls;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// Whole-request ceiling; a `200`/`202` comes straight from the spool or cache.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

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

/// First endpoint that yields an HTTP response, or the per-endpoint transport errors.
// ponytail: every `send()` error counts as "this endpoint is down" (refused, TLS pin mismatch,
// timeout alike) — split by `reqwest::Error` kind if a pin mismatch should stop the fallback.
fn try_endpoints(
    client: &Client,
    contact: &Contact,
    request: impl Fn(&Client, &str) -> reqwest::Result<Response>,
) -> Result<Response, Vec<String>> {
    let mut errors = Vec::new();
    for endpoint in &contact.endpoints {
        match request(client, endpoint) {
            Ok(resp) => return Ok(resp),
            Err(e) => errors.push(format!("{endpoint}: {}", root_cause(&e))),
        }
    }
    Err(errors)
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
fn error_message(status: reqwest::StatusCode, body: &str) -> String {
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
    envelope: &Envelope,
) -> anyhow::Result<SendOutcome> {
    if contact.endpoints.is_empty() {
        bail!("contact {} has no endpoints", contact.name);
    }
    let question: Payload =
        serde_json::from_str(&envelope.raw).context("envelope is not a payload")?;
    let client = http_client(identity, contact)?;
    let resp = match try_endpoints(&client, contact, |c, ep| {
        c.post(endpoint_url(ep, "/v1/questions"))
            .header("content-type", "application/json")
            .header(SIGNATURE_HEADER, &envelope.sig)
            .body(envelope.raw.clone().into_bytes())
            .send()
    }) {
        Ok(resp) => resp,
        Err(errors) => return Ok(SendOutcome::Offline { errors }),
    };
    let status = resp.status();
    match status.as_u16() {
        200 => {
            let sig = resp
                .headers()
                .get(SIGNATURE_HEADER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
                .context("200 answer without X-Owl-Signature header")?;
            let raw = resp.text().context("reading answer body")?;
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
            let body: Value = resp.json().context("202 body is not JSON")?;
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
        429 => {
            let retry_after_secs = resp
                .headers()
                .get(RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse().ok());
            Ok(SendOutcome::RateLimited { retry_after_secs })
        }
        _ => {
            let body = resp.text().unwrap_or_default();
            bail!("{}", error_message(status, &body))
        }
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
pub fn fetch_outbox(identity: &Identity, contact: &Contact) -> anyhow::Result<Vec<Envelope>> {
    if contact.endpoints.is_empty() {
        bail!("contact {} has no endpoints", contact.name);
    }
    let client = http_client(identity, contact)?;
    let resp = try_endpoints(&client, contact, |c, ep| {
        c.get(endpoint_url(ep, "/v1/outbox")).send()
    })
    .map_err(|errors| anyhow::anyhow!("offline: {}", errors.join("; ")))?;
    let status = resp.status();
    if status != 200 {
        let body = resp.text().unwrap_or_default();
        bail!("{}", error_message(status, &body));
    }
    let items: Vec<Envelope> = resp.json().context("outbox body is not [{raw, sig}]")?;
    Ok(items)
}

/// `POST /v1/outbox/{id}/ack`: `204` → ok; anything else (including offline) → error.
pub fn ack(identity: &Identity, contact: &Contact, id: &str) -> anyhow::Result<()> {
    if contact.endpoints.is_empty() {
        bail!("contact {} has no endpoints", contact.name);
    }
    let client = http_client(identity, contact)?;
    let resp = try_endpoints(&client, contact, |c, ep| {
        c.post(endpoint_url(ep, &format!("/v1/outbox/{id}/ack")))
            .send()
    })
    .map_err(|errors| anyhow::anyhow!("offline: {}", errors.join("; ")))?;
    let status = resp.status();
    if status != 204 {
        let body = resp.text().unwrap_or_default();
        bail!("{}", error_message(status, &body));
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
        let s = reqwest::StatusCode::CONFLICT;
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

    #[test]
    fn no_endpoints_is_a_plain_error_not_offline() {
        let (a, b) = (Identity::from_seed([1; 32]), Identity::from_seed([2; 32]));
        let contact = contact_for(&b, &[]);
        let env = Envelope::sign(&Payload::question("owl:a", "owl:b", "p", "f", "why?"), &a);
        let err = send_question(&a, &contact, &env).unwrap_err().to_string();
        assert!(err.contains("no endpoints"), "{err}");
        let err = fetch_outbox(&a, &contact).unwrap_err().to_string();
        assert!(err.contains("no endpoints"), "{err}");
        let err = ack(&a, &contact, "x").unwrap_err().to_string();
        assert!(err.contains("no endpoints"), "{err}");
    }

    #[test]
    fn closed_ports_are_offline_with_one_error_per_endpoint() {
        let (a, b) = (Identity::from_seed([1; 32]), Identity::from_seed([2; 32]));
        let closed = |_: ()| {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().to_string()
        };
        let (p1, p2) = (closed(()), closed(()));
        let contact = contact_for(&b, &[&p1, &p2]);
        let env = Envelope::sign(&Payload::question("owl:a", "owl:b", "p", "f", "why?"), &a);
        match send_question(&a, &contact, &env).unwrap() {
            SendOutcome::Offline { errors } => {
                assert_eq!(errors.len(), 2, "{errors:?}");
                assert!(errors[0].starts_with(&p1), "{errors:?}");
                assert!(errors[1].starts_with(&p2), "{errors:?}");
            }
            other => panic!("expected Offline, got {other:?}"),
        }
        let err = fetch_outbox(&a, &contact).unwrap_err().to_string();
        assert!(err.starts_with("offline: "), "{err}");
        let err = ack(&a, &contact, "x").unwrap_err().to_string();
        assert!(err.starts_with("offline: "), "{err}");
    }
}
