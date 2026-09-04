//! Daemon HTTP API (§7): axum router, handlers, per-peer token bucket, replay window.
//!
//! Peer identity comes from the transport layer: `PeerAcceptor` wraps axum-server's
//! `RustlsAcceptor`, reads the client certificate after the handshake and attaches a
//! `PeerId` extension to every request on that connection; the iroh listener
//! (`crate::iroh`) attaches the same extension from the connection's key. Card routes accept
//! `PeerId(None)` (unpinned clients); every `/v1/*` route requires a fingerprint.
//!
//! `/v1/local/{fingerprint}/{*rest}` is the owner's own forward route: the CLI (a separate
//! process, which must not bind a second iroh endpoint) asks the daemon to replay a request
//! to a peer over iroh; a local failure is `502 {"error": "iroh: …"}`.
//!
//! Every rejected input is a `4xx` with a JSON `{"error": "..."}` body naming the problem.

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, Extension, Path, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::AddExtension;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use axum_server::accept::Accept;
use axum_server::tls_rustls::RustlsAcceptor;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tower::Layer;

use crate::config::Config;
use crate::contacts::{ContactBook, Mode, Policy};
use crate::envelope::{self, Kind, Payload, REPLAY_WINDOW_SECS, SeenIds};
use crate::identity::{self, Identity};
use crate::spool::{Dir, Record, Spool};

pub const SIGNATURE_HEADER: &str = "x-owl-signature";
/// A2A protocol version the card claims to speak.
pub const A2A_PROTOCOL_VERSION: &str = "0.3.0";
/// Largest request body accepted (a question is a few hundred bytes; 413 beyond this).
pub const MAX_BODY_BYTES: usize = 64 * 1024;

/// Fingerprint of the client certificate on this connection; `None` = unpinned client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerId(pub Option<String>);

/// Emitted after a question has been written to `inbox/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spooled {
    pub id: String,
    pub peer: String,
    /// `consent` (no policy yet) or `pending`.
    pub state: String,
    /// The peer's policy is `auto`: the scheduler should draft + send without a human.
    pub auto: bool,
}

/// Emitted after the pull loop has stored a peer's answer under `asks/` (OWL-008 calls
/// `on_answer_ingested`); the daemon turns it into an "answer from <peer>" notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswerIngested {
    /// Id of the answered ask record.
    pub id: String,
    /// Fingerprint of the answering peer.
    pub peer: String,
    /// Path the original question was about.
    pub path: String,
}

/// Everything the request handlers (and the pull loop) report to the daemon loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonEvent {
    Question(Spooled),
    Answer(AnswerIngested),
}

pub struct AppState {
    pub home: PathBuf,
    /// Working directory used for the repo contact provider.
    pub cwd: PathBuf,
    pub config: Config,
    pub identity: Identity,
    pub spool: Spool,
    pub seen: Mutex<SeenIds>,
    pub buckets: Mutex<HashMap<String, Bucket>>,
    /// Set by the daemon once the listener is bound; used for the card `url`.
    pub bound: OnceLock<SocketAddr>,
    /// The daemon's iroh endpoint (set by the daemon; absent in unit tests): the card's
    /// `iroh` block and the forward route read it.
    pub iroh: OnceLock<iroh::Endpoint>,
    /// Daemon event channel: notifications today; the auto-accept scheduler (OWL-008)
    /// consumes the same `Question` events.
    pub on_spooled: Option<tokio::sync::mpsc::UnboundedSender<DaemonEvent>>,
    started: Instant,
}

impl AppState {
    pub fn new(
        home: PathBuf,
        cwd: PathBuf,
        config: Config,
        identity: Identity,
        on_spooled: Option<tokio::sync::mpsc::UnboundedSender<DaemonEvent>>,
    ) -> anyhow::Result<AppState> {
        let spool = Spool::new(&home)?;
        let seen = SeenIds::load(&home, envelope::now_unix())?;
        Ok(AppState {
            home,
            cwd,
            config,
            identity,
            spool,
            seen: Mutex::new(seen),
            buckets: Mutex::new(HashMap::new()),
            bound: OnceLock::new(),
            iroh: OnceLock::new(),
            on_spooled,
            started: Instant::now(),
        })
    }

    pub fn fingerprint(&self) -> String {
        identity::fingerprint(&self.identity.verifying_key())
    }

    /// Seconds since the state was created; the token buckets' clock.
    fn clock(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    /// Fresh contact book on every call so `owl allow` / `owl deny` apply without a restart.
    // ponytail: the TLS allowed-key set is a snapshot from daemon start — a newly added
    // contact still needs a daemon restart to get through the handshake (reload in OWL-012).
    pub(crate) fn contacts(&self) -> anyhow::Result<ContactBook> {
        ContactBook::load(&self.home, &self.cwd)
    }
}

/// Token bucket: capacity = `rate` tokens, refilled at `rate` per hour.
#[derive(Debug, Clone, PartialEq)]
pub struct Bucket {
    tokens: f64,
    last: f64,
}

impl Bucket {
    pub fn full(rate: u64, now: f64) -> Bucket {
        Bucket {
            tokens: rate as f64,
            last: now,
        }
    }

    /// Takes one token. `Err(secs)` = rate limited, retry after `secs` (≥ 1).
    pub fn take(&mut self, rate: u64, now: f64) -> Result<(), u64> {
        if rate == 0 {
            return Err(3600);
        }
        let cap = rate as f64;
        let per_sec = cap / 3600.0;
        let elapsed = (now - self.last).max(0.0);
        self.tokens = (self.tokens + elapsed * per_sec).min(cap);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            return Ok(());
        }
        let wait = (1.0 - self.tokens) / per_sec;
        Err((wait.ceil() as u64).max(1))
    }
}

/// JSON error body `{"error": msg}` with a status (and `Retry-After` for 429).
#[derive(Debug, PartialEq, Eq)]
pub struct ApiError {
    pub status: StatusCode,
    pub error: String,
    pub retry_after: Option<u64>,
}

impl ApiError {
    fn new(status: StatusCode, error: impl Into<String>) -> ApiError {
        ApiError {
            status,
            error: error.into(),
            retry_after: None,
        }
    }
    fn bad_request(error: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::BAD_REQUEST, error)
    }
    fn unavailable() -> ApiError {
        ApiError::new(StatusCode::FORBIDDEN, "unavailable")
    }
    fn not_found() -> ApiError {
        ApiError::new(StatusCode::NOT_FOUND, "not found")
    }
    /// Storage failures: the only 500 the API can produce, never reachable from bad input.
    fn storage(e: anyhow::Error) -> ApiError {
        tracing::error!(error = %format!("{e:#}"), "storage failure");
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "storage error")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut resp = (self.status, Json(json!({ "error": self.error }))).into_response();
        if let Some(secs) = self.retry_after
            && let Ok(v) = HeaderValue::from_str(&secs.to_string())
        {
            resp.headers_mut().insert(header::RETRY_AFTER, v);
        }
        resp
    }
}

type ApiResult<T> = Result<T, ApiError>;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/.well-known/agent-card.json", get(card))
        .route("/.well-known/agent.json", get(card))
        .route("/v1/questions", post(post_question))
        .route("/v1/outbox", get(get_outbox))
        .route("/v1/outbox/{id}/ack", post(ack_outbox))
        .route("/v1/local/{fingerprint}/{*rest}", any(forward))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

async fn not_found() -> ApiError {
    ApiError::not_found()
}

async fn method_not_allowed() -> ApiError {
    ApiError::new(StatusCode::METHOD_NOT_ALLOWED, "method not allowed")
}

/// Body extraction failures as JSON: 413 over `MAX_BODY_BYTES`, 400 for a broken stream.
fn body_bytes(body: Result<Bytes, BytesRejection>) -> ApiResult<Bytes> {
    body.map_err(|e| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "body too large")
        } else {
            ApiError::bad_request("unreadable body")
        }
    })
}

/// Inbox state for a spooled question and whether the scheduler should auto-answer it.
pub fn record_state(mode: Option<Mode>) -> (&'static str, bool) {
    match mode {
        None => ("consent", false),
        Some(Mode::Auto) => ("pending", true),
        Some(Mode::Manual) | Some(Mode::Never) => ("pending", false),
    }
}

/// Only a peer the owner has already allowed (`manual`/`auto`) may receive a cached answer.
pub fn may_read_cache(mode: Option<Mode>) -> bool {
    matches!(mode, Some(Mode::Manual) | Some(Mode::Auto))
}

/// A2A-shaped agent card (§7). Served to pinned and unpinned clients alike.
pub fn card_json(state: &AppState) -> Value {
    let pk = state.identity.verifying_key();
    let host = state
        .config
        .endpoints
        .first()
        .cloned()
        .or_else(|| state.bound.get().map(|a| a.to_string()))
        .unwrap_or_else(|| state.config.listen.clone());
    let name = if state.config.name.is_empty() {
        "owlpost".to_string()
    } else {
        state.config.name.clone()
    };
    // The iroh endpoint id is the identity key, so `iroh.id` repeats `owlpost.pubkey`.
    let iroh = state.iroh.get().map(|ep| {
        json!({
            "id": identity::pubkey_string(&pk),
            "relay": crate::iroh::home_relay(ep),
        })
    });
    json!({
        "name": name,
        "description": format!("owlpost agent of {name}: answers questions about their code"),
        "url": format!("https://{host}/"),
        "version": env!("CARGO_PKG_VERSION"),
        "protocolVersion": A2A_PROTOCOL_VERSION,
        "capabilities": { "streaming": false, "pushNotifications": false },
        "skills": [],
        "owlpost": {
            "fingerprint": identity::fingerprint(&pk),
            "pubkey": identity::pubkey_string(&pk),
            "protocol": 1,
            "responds": state.config.responder.enabled,
            "harness": state.config.responder.harness,
        },
        "iroh": iroh,
    })
}

async fn card(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(card_json(&state))
}

fn require_peer(peer: &PeerId) -> ApiResult<&str> {
    peer.0
        .as_deref()
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "client certificate required"))
}

fn signature_header(headers: &HeaderMap) -> ApiResult<ed25519_dalek::Signature> {
    let raw = headers
        .get(SIGNATURE_HEADER)
        .ok_or_else(|| ApiError::bad_request("missing X-Owl-Signature header"))?;
    let text = raw
        .to_str()
        .map_err(|_| ApiError::bad_request("malformed X-Owl-Signature header"))?;
    identity::parse_sig(text).map_err(|_| ApiError::bad_request("malformed X-Owl-Signature header"))
}

fn str_field<'a>(obj: &'a serde_json::Map<String, Value>, key: &str) -> ApiResult<&'a str> {
    match obj.get(key) {
        Some(Value::String(s)) => Ok(s),
        _ => Err(ApiError::bad_request(format!("missing {key}"))),
    }
}

/// Shape checks with a named error for each field, then the typed parse.
fn validate_question(value: &Value, caller: &str, me: &str) -> ApiResult<Payload> {
    let obj = value
        .as_object()
        .ok_or_else(|| ApiError::bad_request("payload must be a JSON object"))?;
    if obj.get("v") != Some(&Value::from(1)) {
        return Err(ApiError::bad_request("unsupported v"));
    }
    let id = str_field(obj, "id")?;
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err(ApiError::bad_request("malformed id"));
    }
    if str_field(obj, "type")? != "question" {
        return Err(ApiError::bad_request("type must be question"));
    }
    if str_field(obj, "from")? != caller {
        return Err(ApiError::bad_request(
            "from does not match the client certificate",
        ));
    }
    if str_field(obj, "to")? != me {
        return Err(ApiError::bad_request("to is not this daemon"));
    }
    str_field(obj, "ts")?;
    let body = obj
        .get("body")
        .and_then(Value::as_object)
        .ok_or_else(|| ApiError::bad_request("body must be an object"))?;
    str_field(body, "project").map_err(|_| ApiError::bad_request("missing body.project"))?;
    str_field(body, "path").map_err(|_| ApiError::bad_request("missing body.path"))?;
    let question =
        str_field(body, "question").map_err(|_| ApiError::bad_request("missing body.question"))?;
    if question.trim().is_empty() {
        return Err(ApiError::bad_request("body.question is empty"));
    }
    serde_json::from_value(value.clone())
        .map_err(|e| ApiError::bad_request(format!("bad schema: {e}")))
}

fn policy_of(book: &ContactBook, fp: &str) -> Option<Policy> {
    book.policy_for(fp).cloned()
}

/// `POST /v1/questions` — check order: JSON → signature header → contact → signature →
/// schema (`from` = caller) → replay window → duplicate id → rate limit → policy →
/// cache (allowed peers only) → spool. Nothing is written before every check has passed.
async fn post_question(
    State(state): State<Arc<AppState>>,
    Extension(peer): Extension<PeerId>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> ApiResult<Response> {
    let caller = require_peer(&peer)?.to_string();
    let body = body_bytes(body)?;
    let value: Value =
        serde_json::from_slice(&body).map_err(|_| ApiError::bad_request("body is not JSON"))?;
    let sig = signature_header(&headers)?;
    let book = state.contacts().map_err(ApiError::storage)?;
    let contact = book
        .contacts
        .iter()
        .find(|c| c.fingerprint == caller)
        .ok_or_else(|| ApiError::new(StatusCode::FORBIDDEN, "unknown peer"))?;
    let pubkey = identity::parse_pubkey(&contact.pubkey)
        .map_err(|_| ApiError::new(StatusCode::FORBIDDEN, "unknown peer"))?;
    if !identity::verify(&pubkey, &body, &sig) {
        return Err(ApiError::bad_request("bad signature"));
    }
    let payload = validate_question(&value, &caller, &state.fingerprint())?;
    let now = envelope::now_unix();
    if envelope::parse_rfc3339_to_unix(&payload.ts).is_none() {
        return Err(ApiError::bad_request("malformed ts"));
    }
    if !envelope::is_fresh(&payload.ts, now, REPLAY_WINDOW_SECS) {
        return Err(ApiError::bad_request("stale ts"));
    }
    if state
        .seen
        .lock()
        .expect("seen-ids lock")
        .ids
        .iter()
        .any(|(i, _)| *i == payload.id)
    {
        return Err(ApiError::new(StatusCode::CONFLICT, "duplicate id"));
    }
    let policy = policy_of(&book, &caller);
    let rate = policy
        .as_ref()
        .and_then(|p| p.rate_limit_per_hour)
        .map_or(state.config.rate_limit_per_peer_per_hour, u64::from);
    {
        let clock = state.clock();
        let mut buckets = state.buckets.lock().expect("buckets lock");
        let bucket = buckets
            .entry(caller.clone())
            .or_insert_with(|| Bucket::full(rate, clock));
        if let Err(secs) = bucket.take(rate, clock) {
            return Err(ApiError {
                retry_after: Some(secs),
                ..ApiError::new(StatusCode::TOO_MANY_REQUESTS, "rate limited")
            });
        }
    }
    if !state.config.responder.enabled || policy.as_ref().is_some_and(|p| p.mode == Mode::Never) {
        return Err(ApiError::unavailable());
    }
    let (project, path, question) = match &payload.body {
        envelope::Body::Question {
            project,
            path,
            question,
        } => (project.as_str(), path.as_str(), question.as_str()),
        envelope::Body::Answer { .. } => {
            return Err(ApiError::bad_request("type must be question"));
        }
    };
    let hash = envelope::question_hash(project, path, question);
    let mode = policy.as_ref().map(|p| p.mode);
    if may_read_cache(mode)
        && let Some(cached) = state.spool.cache_get(&hash).map_err(ApiError::storage)?
    {
        remember(&state, &payload.id, now)?;
        tracing::info!(peer = %caller, id = %payload.id, "cache hit");
        return Ok(answer_response(&cached.raw, &cached.sig));
    }
    let (record_state, auto) = record_state(mode);
    let sig_text = headers
        .get(SIGNATURE_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let record = Record {
        raw: String::from_utf8_lossy(&body).into_owned(),
        sig: sig_text,
        state: record_state.to_string(),
        seen: false,
        received_at: envelope::unix_to_rfc3339(now),
        draft: None,
        meta: json!({ "peer": caller, "hash": hash }),
    };
    state
        .spool
        .put(Dir::Inbox, &payload.id, &record)
        .map_err(ApiError::storage)?;
    remember(&state, &payload.id, now)?;
    tracing::info!(peer = %caller, id = %payload.id, state = record_state, "question spooled");
    on_question_spooled(
        &state,
        Spooled {
            id: payload.id.clone(),
            peer: caller,
            state: record_state.to_string(),
            auto,
        },
    );
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "status": "accepted", "id": payload.id })),
    )
        .into_response())
}

/// Records the id in the replay window (memory + `seen-ids.txt`).
fn remember(state: &AppState, id: &str, now: u64) -> ApiResult<()> {
    let mut seen = state.seen.lock().expect("seen-ids lock");
    seen.insert(id, now);
    seen.save(&state.home).map_err(ApiError::storage)
}

/// Question arrival hook: forwards to the daemon loop (notification now, scheduler in OWL-008).
// ponytail: OWL-008 extends the daemon's channel consumer with the draft + send scheduler.
pub fn on_question_spooled(state: &AppState, event: Spooled) {
    if let Some(tx) = &state.on_spooled {
        let _ = tx.send(DaemonEvent::Question(event));
    }
}

/// Answer ingestion hook for the pull loop (OWL-008): the daemon notifies "answer from <peer>".
pub fn on_answer_ingested(state: &AppState, event: AnswerIngested) {
    if let Some(tx) = &state.on_spooled {
        let _ = tx.send(DaemonEvent::Answer(event));
    }
}

/// `200` with the signed answer payload as the body and the signature in the header (§6).
fn answer_response(raw: &str, sig: &str) -> Response {
    let mut resp = (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        raw.to_string(),
    )
        .into_response();
    if let Ok(v) = HeaderValue::from_str(sig) {
        resp.headers_mut().insert(SIGNATURE_HEADER, v);
    }
    resp
}

fn payload_to(rec: &Record) -> Option<String> {
    serde_json::from_str::<Payload>(&rec.raw)
        .ok()
        .filter(|p| p.kind == Kind::Answer)
        .map(|p| p.to)
}

/// `GET /v1/outbox` — answers whose `to` is the caller, as `[{raw, sig}]`.
async fn get_outbox(
    State(state): State<Arc<AppState>>,
    Extension(peer): Extension<PeerId>,
) -> ApiResult<Json<Value>> {
    let caller = require_peer(&peer)?;
    let items: Vec<Value> = list_lenient(&state.spool, Dir::Outbox)?
        .into_iter()
        .filter(|(_, r)| payload_to(r).as_deref() == Some(caller))
        .map(|(_, r)| json!({ "raw": r.raw, "sig": r.sig }))
        .collect();
    Ok(Json(Value::Array(items)))
}

/// `Spool::list_lenient` with the storage error mapped for the handler.
fn list_lenient(spool: &Spool, dir: Dir) -> ApiResult<Vec<(String, Record)>> {
    spool.list_lenient(dir).map_err(ApiError::storage)
}

/// `POST /v1/outbox/{id}/ack` — `204` and the record moves to `done/` (state `acked`);
/// `404` when the id is unknown or addressed to someone else.
async fn ack_outbox(
    State(state): State<Arc<AppState>>,
    Extension(peer): Extension<PeerId>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let caller = require_peer(&peer)?;
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err(ApiError::not_found());
    }
    let rec = match state.spool.get(Dir::Outbox, &id) {
        Ok(Some(rec)) => rec,
        Ok(None) => return Err(ApiError::not_found()),
        Err(e) => {
            // A corrupt file is nobody's answer: 404, like the lenient listing skips it.
            tracing::warn!(id = %id, error = %format!("{e:#}"), "corrupt outbox record");
            return Err(ApiError::not_found());
        }
    };
    if payload_to(&rec).as_deref() != Some(caller) {
        return Err(ApiError::not_found());
    }
    // Move first: a failed move must not leave an `acked` record sitting in the outbox.
    state
        .spool
        .move_to(Dir::Outbox, &id, Dir::Done)
        .and_then(|_| state.spool.set_state(Dir::Done, &id, "acked"))
        .map_err(ApiError::storage)?;
    tracing::info!(peer = %caller, id = %id, "answer acked");
    Ok(StatusCode::NO_CONTENT)
}

/// The three peer paths the forward route replays (`v1/questions`, `v1/outbox`,
/// `v1/outbox/{id}/ack`); anything else is `404`.
pub fn forwardable(rest: &str) -> bool {
    match rest {
        "v1/questions" | "v1/outbox" => true,
        _ => rest
            .strip_prefix("v1/outbox/")
            .and_then(|r| r.strip_suffix("/ack"))
            .is_some_and(|id| {
                !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            }),
    }
}

/// Request headers the forward route replays and response headers it returns.
fn forwarded_header(name: &str) -> bool {
    name.starts_with("x-owl-") || name == "content-type" || name == "retry-after"
}

fn iroh_unavailable(reason: impl std::fmt::Display) -> ApiError {
    ApiError::new(StatusCode::BAD_GATEWAY, format!("iroh: {reason}"))
}

/// `ANY /v1/local/{fingerprint}/{*rest}` — owner only (the caller's key is this daemon's
/// key). Replays method, `X-Owl-*` headers and body to the contact over iroh and returns
/// the peer's status, headers and body verbatim. `502 {"error": "iroh: …"}` means the peer
/// was not reached (no contact, no endpoint, dial timeout): the client tries `endpoints`.
async fn forward(
    State(state): State<Arc<AppState>>,
    Extension(peer): Extension<PeerId>,
    Path((fingerprint, rest)): Path<(String, String)>,
    method: Method,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> ApiResult<Response> {
    let caller = require_peer(&peer)?;
    if caller != state.fingerprint() {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "owner only"));
    }
    if !forwardable(&rest) {
        return Err(ApiError::not_found());
    }
    let body = body_bytes(body)?;
    let book = state.contacts().map_err(ApiError::storage)?;
    let contact = book
        .contacts
        .iter()
        .find(|c| c.fingerprint == fingerprint)
        .ok_or_else(|| iroh_unavailable(format!("unknown contact {fingerprint}")))?;
    let endpoint = state
        .iroh
        .get()
        .ok_or_else(|| iroh_unavailable("no endpoint"))?;
    let addr = crate::iroh::peer_addr(&contact.pubkey, state.config.relay_urls.as_deref())
        .map_err(|e| iroh_unavailable(format!("{e:#}")))?;
    let replay: HeaderMap = headers
        .iter()
        .filter(|(k, _)| forwarded_header(k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let reply = crate::iroh::request(endpoint, addr, method, &format!("/{rest}"), replay, body)
        .await
        .map_err(|e| {
            tracing::warn!(peer = %fingerprint, error = %format!("{e:#}"), "iroh forward failed");
            iroh_unavailable(format!("{e:#}"))
        })?;
    let mut resp = (
        StatusCode::from_u16(reply.status).unwrap_or(StatusCode::BAD_GATEWAY),
        reply.body,
    )
        .into_response();
    for (k, v) in reply.headers.iter() {
        if forwarded_header(k.as_str()) {
            resp.headers_mut().insert(k.clone(), v.clone());
        }
    }
    Ok(resp)
}

/// TLS acceptor that tags each connection's service with the client's fingerprint.
#[derive(Clone)]
pub struct PeerAcceptor {
    inner: RustlsAcceptor,
}

impl PeerAcceptor {
    pub fn new(inner: RustlsAcceptor) -> PeerAcceptor {
        PeerAcceptor { inner }
    }
}

type AcceptFuture<S, T> = Pin<Box<dyn Future<Output = std::io::Result<(S, T)>> + Send>>;

impl<I, S> Accept<I, S> for PeerAcceptor
where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    S: Send + 'static,
{
    type Stream = <RustlsAcceptor as Accept<I, S>>::Stream;
    type Service = AddExtension<S, PeerId>;
    type Future = AcceptFuture<Self::Stream, Self::Service>;

    fn accept(&self, stream: I, service: S) -> Self::Future {
        let acceptor = self.inner.clone();
        Box::pin(async move {
            let (stream, service) = acceptor.accept(stream, service).await?;
            let peer = match stream.get_ref().1.peer_certificates() {
                Some([cert, ..]) => match crate::tls::peer_fingerprint_from_cert(cert) {
                    Ok(fp) => Some(fp),
                    Err(e) => {
                        // The verifier already accepted this key, so this cannot happen;
                        // fail closed rather than serve an unidentified pinned client.
                        return Err(std::io::Error::other(format!("peer certificate: {e}")));
                    }
                },
                _ => None,
            };
            Ok((stream, Extension(PeerId(peer)).layer(service)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_refills_at_rate_per_hour() {
        let mut b = Bucket::full(3, 0.0);
        assert_eq!(b.take(3, 0.0), Ok(()));
        assert_eq!(b.take(3, 0.0), Ok(()));
        assert_eq!(b.take(3, 0.0), Ok(()));
        // Empty: one token takes 1200 s at 3/h.
        assert_eq!(b.take(3, 0.0), Err(1200));
        assert_eq!(b.take(3, 600.0), Err(600), "half refilled");
        assert_eq!(b.take(3, 1199.5), Err(1), "rounded up, never 0");
        assert_eq!(b.take(3, 1200.0), Ok(()), "refilled after 1200 s");
        assert_eq!(b.take(3, 1200.0), Err(1200));
        // Capacity caps the refill.
        let mut b = Bucket::full(2, 0.0);
        assert_eq!(b.take(2, 100_000.0), Ok(()));
        assert_eq!(b.take(2, 100_000.0), Ok(()));
        assert!(b.take(2, 100_000.0).is_err());
        // Clock going backwards is treated as no time elapsed.
        let mut b = Bucket::full(1, 10.0);
        assert_eq!(b.take(1, 5.0), Ok(()));
        assert_eq!(b.take(1, 5.0), Err(3600));
    }

    #[test]
    fn retry_after_rounds_up_to_a_full_second() {
        let mut b = Bucket::full(3, 0.0);
        for _ in 0..3 {
            assert_eq!(b.take(3, 0.0), Ok(()));
        }
        assert_eq!(b.take(3, 0.5), Err(1200), "1199.5 s rounds up, not down");
        assert_eq!(b.take(3, 1.5), Err(1199), "1198.5 s rounds up to 1199");
        assert_eq!(b.take(3, 2.0), Err(1198), "exact seconds stay exact");
    }

    #[test]
    fn record_state_and_cache_gate_per_policy() {
        assert_eq!(record_state(None), ("consent", false));
        assert_eq!(record_state(Some(Mode::Manual)), ("pending", false));
        assert_eq!(record_state(Some(Mode::Auto)), ("pending", true));
        assert_eq!(record_state(Some(Mode::Never)), ("pending", false));
        assert!(!may_read_cache(None));
        assert!(may_read_cache(Some(Mode::Manual)));
        assert!(may_read_cache(Some(Mode::Auto)));
        assert!(!may_read_cache(Some(Mode::Never)));
    }

    #[test]
    fn zero_rate_always_limits() {
        let mut b = Bucket::full(0, 0.0);
        assert_eq!(b.take(0, 0.0), Err(3600));
        assert_eq!(b.take(0, 99_999.0), Err(3600));
    }

    #[test]
    fn api_error_carries_retry_after_only_when_set() {
        let plain = ApiError::bad_request("x").into_response();
        assert_eq!(plain.status(), StatusCode::BAD_REQUEST);
        assert!(plain.headers().get(header::RETRY_AFTER).is_none());
        let limited = ApiError {
            retry_after: Some(42),
            ..ApiError::new(StatusCode::TOO_MANY_REQUESTS, "rate limited")
        }
        .into_response();
        assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(limited.headers()[header::RETRY_AFTER], "42");
    }

    fn valid() -> Value {
        json!({
            "v": 1,
            "id": "0191c7a0-0000-7000-8000-000000000000",
            "type": "question",
            "from": "owl:aaaa",
            "to": "owl:bbbb",
            "ts": "2026-09-01T10:00:00Z",
            "in_reply_to": null,
            "body": { "project": "p", "path": "f", "question": "why?" }
        })
    }

    fn err_of(v: Value) -> String {
        validate_question(&v, "owl:aaaa", "owl:bbbb")
            .unwrap_err()
            .error
    }

    #[test]
    fn validate_question_names_every_problem() {
        assert!(validate_question(&valid(), "owl:aaaa", "owl:bbbb").is_ok());
        assert_eq!(err_of(json!([1])), "payload must be a JSON object");
        assert_eq!(err_of(json!("x")), "payload must be a JSON object");
        let mut v = valid();
        v["v"] = json!(2);
        assert_eq!(err_of(v), "unsupported v");
        let mut v = valid();
        v.as_object_mut().unwrap().remove("id");
        assert_eq!(err_of(v), "missing id");
        let mut v = valid();
        v["id"] = json!("../x");
        assert_eq!(err_of(v), "malformed id");
        let mut v = valid();
        v["id"] = json!("");
        assert_eq!(err_of(v), "malformed id");
        let mut v = valid();
        v["type"] = json!("answer");
        assert_eq!(err_of(v), "type must be question");
        let mut v = valid();
        v["type"] = json!(7);
        assert_eq!(err_of(v), "missing type");
        let mut v = valid();
        v["from"] = json!("owl:cccc");
        assert_eq!(err_of(v), "from does not match the client certificate");
        let mut v = valid();
        v["to"] = json!("owl:cccc");
        assert_eq!(err_of(v), "to is not this daemon");
        let mut v = valid();
        v.as_object_mut().unwrap().remove("ts");
        assert_eq!(err_of(v), "missing ts");
        let mut v = valid();
        v["body"] = json!("text");
        assert_eq!(err_of(v), "body must be an object");
        let mut v = valid();
        v.as_object_mut().unwrap().remove("body");
        assert_eq!(err_of(v), "body must be an object");
        let mut v = valid();
        v["body"].as_object_mut().unwrap().remove("project");
        assert_eq!(err_of(v), "missing body.project");
        let mut v = valid();
        v["body"].as_object_mut().unwrap().remove("path");
        assert_eq!(err_of(v), "missing body.path");
        let mut v = valid();
        v["body"].as_object_mut().unwrap().remove("question");
        assert_eq!(err_of(v), "missing body.question");
        let mut v = valid();
        v["body"]["question"] = json!(3);
        assert_eq!(err_of(v), "missing body.question");
        let mut v = valid();
        v["body"]["question"] = json!("  \n ");
        assert_eq!(err_of(v), "body.question is empty");
        let mut v = valid();
        v["in_reply_to"] = json!(5);
        assert!(err_of(v).starts_with("bad schema: "));
    }

    #[test]
    fn card_uses_endpoint_then_bound_addr_then_listen() {
        let home = tempfile::tempdir().unwrap();
        let id = Identity::from_seed([3u8; 32]);
        let mut cfg = Config {
            listen: "127.0.0.1:0".into(),
            ..Default::default()
        };
        cfg.responder.enabled = false;
        let state = AppState::new(
            home.path().to_path_buf(),
            home.path().to_path_buf(),
            cfg.clone(),
            Identity::from_seed([3u8; 32]),
            None,
        )
        .unwrap();
        let card = card_json(&state);
        assert_eq!(card["url"], "https://127.0.0.1:0/");
        assert_eq!(card["name"], "owlpost");
        assert_eq!(card["owlpost"]["responds"], false);
        assert_eq!(card["owlpost"]["protocol"], 1);
        assert_eq!(card["owlpost"]["harness"], "claude");
        assert_eq!(
            card["owlpost"]["fingerprint"],
            identity::fingerprint(&id.verifying_key())
        );
        assert_eq!(
            card["owlpost"]["pubkey"],
            identity::pubkey_string(&id.verifying_key())
        );
        assert_eq!(card["protocolVersion"], A2A_PROTOCOL_VERSION);
        assert_eq!(card["capabilities"]["streaming"], false);
        assert_eq!(card["capabilities"]["pushNotifications"], false);
        assert_eq!(card["skills"], json!([]));
        assert_eq!(card["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(card["iroh"], Value::Null, "no endpoint outside the daemon");
        state.bound.set("127.0.0.1:4321".parse().unwrap()).unwrap();
        assert_eq!(card_json(&state)["url"], "https://127.0.0.1:4321/");
        cfg.endpoints = vec!["b.example.org:7411".into()];
        cfg.name = "Bea".into();
        let state = AppState::new(
            home.path().to_path_buf(),
            home.path().to_path_buf(),
            cfg,
            Identity::from_seed([3u8; 32]),
            None,
        )
        .unwrap();
        let card = card_json(&state);
        assert_eq!(card["url"], "https://b.example.org:7411/");
        assert_eq!(card["name"], "Bea");
        assert_eq!(card["owlpost"]["responds"], false);
    }

    #[test]
    fn forwardable_is_exactly_the_three_peer_paths() {
        assert!(forwardable("v1/questions"));
        assert!(forwardable("v1/outbox"));
        assert!(forwardable("v1/outbox/0191c7a0-0000-7000-8000-000000000000/ack"));
        assert!(forwardable("v1/outbox/abc/ack"));
        for bad in [
            "",
            "v1",
            "v1/questions/",
            "v1/questions/x",
            "v1/outbox/",
            "v1/outbox/abc",
            "v1/outbox//ack",
            "v1/outbox/../ack",
            "v1/outbox/a b/ack",
            "v1/outbox/abc/ack/",
            "v1/local/owl:x/v1/questions",
            ".well-known/agent-card.json",
            "V1/questions",
        ] {
            assert!(!forwardable(bad), "{bad:?}");
        }
        assert!(forwarded_header("x-owl-signature"));
        assert!(forwarded_header("content-type"));
        assert!(forwarded_header("retry-after"));
        assert!(!forwarded_header("authorization"));
        assert!(!forwarded_header("host"));
        assert!(!forwarded_header("xowl-signature"));
    }
}
