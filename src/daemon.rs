//! Daemon run loop: the mTLS listener, the iroh listener (`crate::iroh`), the pull loop
//! (`crate::pull`), the auto-accept scan (`crate::auto`) and the session wake lease loop
//! (`crate::route`, OWL-033) as sibling tasks; the event consumer notifies and triggers
//! auto-accept.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use axum_server::Handle;
use axum_server::tls_rustls::{RustlsAcceptor, RustlsConfig};
use tokio::task::JoinHandle;

use crate::config::Config;
use crate::contacts::ContactBook;
use crate::identity::{self, Identity};
use crate::notify::{self, Kind};
use crate::server::{AppState, DaemonEvent, PeerAcceptor};
use crate::spool::Dir;
use crate::tls::{self, AllowedKeys};

pub const ADDR_FILE: &str = "daemon.addr";

/// A daemon running inside this process.
pub struct Running {
    pub addr: SocketAddr,
    pub state: Arc<AppState>,
    handle: Handle<SocketAddr>,
    task: JoinHandle<std::io::Result<()>>,
    /// The pull loop; aborted on `shutdown` / `wait`.
    pull: JoinHandle<()>,
    /// The auto-accept scan; aborted with the pull loop.
    auto: JoinHandle<()>,
    /// The session wake lease loop (`crate::route`, OWL-033); aborted with the pull loop.
    lease: JoinHandle<()>,
    /// The iroh endpoint (closed on `shutdown` / `wait`) and its accept loop.
    iroh: iroh::Endpoint,
    accept: JoinHandle<()>,
}

impl std::fmt::Debug for Running {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Running").field("addr", &self.addr).finish()
    }
}

impl Running {
    /// Stops accepting, drops open connections, closes the iroh endpoint and stops the pull
    /// loop.
    pub fn shutdown(&self) {
        self.handle.shutdown();
        self.pull.abort();
        self.auto.abort();
        self.lease.abort();
        self.accept.abort();
        let endpoint = self.iroh.clone();
        tokio::spawn(async move { endpoint.close().await });
    }

    /// Waits for the listener task to end (after `shutdown`, or on a listener error); the pull
    /// loop and the iroh endpoint are stopped with it.
    pub async fn wait(self) -> anyhow::Result<()> {
        let result = self.task.await.context("listener task");
        self.pull.abort();
        self.auto.abort();
        self.lease.abort();
        self.accept.abort();
        self.iroh.close().await;
        result??;
        Ok(())
    }

    /// The daemon's iroh endpoint.
    pub fn iroh(&self) -> &iroh::Endpoint {
        &self.iroh
    }

    /// True while the pull loop task is alive.
    pub fn pull_running(&self) -> bool {
        !self.pull.is_finished()
    }
}

/// Client keys allowed through the TLS handshake: every contact in the merged book, plus
/// the owner's own key (the CLI uses it for the iroh forward route).
pub fn allowed_keys(book: &ContactBook, owner: &Identity) -> AllowedKeys {
    book.contacts
        .iter()
        .filter_map(|c| identity::parse_pubkey(&c.pubkey).ok())
        .map(|pk| *pk.as_bytes())
        .chain(std::iter::once(*owner.verifying_key().as_bytes()))
        .collect()
}

/// Binds `config.listen` (port 0 allowed) and serves the API on the tokio runtime.
pub async fn spawn(home: &Path, config: Config) -> anyhow::Result<Running> {
    let identity = Identity::load(home)?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let book = ContactBook::load(home, &cwd)?;
    let listen: SocketAddr = config
        .listen
        .parse()
        .with_context(|| format!("config.listen {:?} is not host:port", config.listen))?;
    let tls_config = tls::server_config(&identity, allowed_keys(&book, &identity), true)?;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<DaemonEvent>();
    let scheduler = crate::auto::Scheduler::new();
    let state = Arc::new(AppState::new(
        home.to_path_buf(),
        cwd,
        config,
        identity,
        Some(tx),
    )?);
    let events_state = state.clone();
    let events_scheduler = scheduler.clone();
    tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if let DaemonEvent::Question(q) = &ev
                && q.auto
            {
                events_scheduler.spawn(events_state.clone(), q.id.clone());
            }
            handle_event(&events_state, ev);
        }
    });
    let endpoint =
        crate::iroh::endpoint(&state.identity, state.config.relay_urls.as_deref()).await?;
    let _ = state.iroh.set(endpoint.clone());
    let accept = tokio::spawn(crate::iroh::accept_loop(endpoint.clone(), state.clone()));
    let app = crate::server::router(state.clone());
    let acceptor = PeerAcceptor::new(RustlsAcceptor::new(RustlsConfig::from_config(tls_config)));
    let handle = Handle::new();
    let server = axum_server::bind(listen)
        .acceptor(acceptor)
        .handle(handle.clone());
    let task = tokio::spawn(server.serve(app.into_make_service()));
    let Some(addr) = handle.listening().await else {
        let err = match task.await {
            Ok(Err(e)) => anyhow::Error::from(e),
            Ok(Ok(())) => anyhow::anyhow!("listener exited before binding"),
            Err(e) => anyhow::Error::from(e),
        };
        accept.abort();
        endpoint.close().await;
        return Err(err.context(format!("binding {listen}")));
    };
    let _ = state.bound.set(addr);
    tracing::info!(%addr, fingerprint = %state.fingerprint(), iroh = %endpoint.id().fmt_short(), "listening");
    let pull = tokio::spawn(crate::pull::run_loop(state.clone()));
    let auto = tokio::spawn(scheduler.run_scan(state.clone()));
    let lease = tokio::spawn(crate::route::lease_loop(state.clone()));
    Ok(Running {
        addr,
        state,
        handle,
        task,
        pull,
        auto,
        lease,
        iroh: endpoint,
        accept,
    })
}

/// One daemon-loop event: log it and fire the OS notification (§11).
fn handle_event(state: &AppState, ev: DaemonEvent) {
    match ev {
        DaemonEvent::Question(q) => {
            tracing::info!(id = %q.id, peer = %q.peer, state = %q.state, auto = q.auto, "spooled");
            let path = question_path(state, &q.id);
            notify::notify(
                &state.config,
                Kind::Question,
                &peer_name(state, &q.peer),
                &path,
            );
        }
        DaemonEvent::Answer(a) => {
            tracing::info!(id = %a.id, peer = %a.peer, path = %a.path, "answer ingested");
            notify::notify(
                &state.config,
                Kind::Answer,
                &peer_name(state, &a.peer),
                &a.path,
            );
        }
        DaemonEvent::Declined(d) => {
            tracing::info!(id = %d.id, peer = %d.peer, "question declined");
            notify::notify(&state.config, Kind::Declined, &peer_name(state, &d.peer), "-");
        }
    }
}

/// Display name for a fingerprint: the contact's name, else the fingerprint itself.
pub fn peer_name(state: &AppState, fingerprint: &str) -> String {
    ContactBook::load(&state.home, &state.cwd)
        .ok()
        .and_then(|book| {
            book.contacts
                .into_iter()
                .find(|c| c.fingerprint == fingerprint)
                .map(|c| c.name)
        })
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| fingerprint.to_string())
}

/// `body.path` of the spooled question `id`; `"-"` for a repo-level question, `"?"` when the
/// record cannot be read.
fn question_path(state: &AppState, id: &str) -> String {
    let rec = match state.spool.get(Dir::Inbox, id) {
        Ok(Some(rec)) => rec,
        _ => return "?".to_string(),
    };
    match serde_json::from_str::<crate::envelope::Payload>(&rec.raw) {
        Ok(crate::envelope::Payload {
            body: crate::envelope::Body::Question { path, .. },
            ..
        }) => path.unwrap_or_else(|| "-".to_string()),
        _ => "?".to_string(),
    }
}

/// Atomically writes `<home>/<name>`: `<name>.tmp` + rename, like the spool; a failed rename
/// removes the temp file.
pub fn write_atomic(home: &Path, name: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let path = home.join(name);
    let tmp = home.join(format!("{name}.tmp"));
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("renaming to {}", path.display()));
    }
    Ok(())
}

/// Atomically writes `<home>/daemon.addr` (`host:port\n`).
pub fn write_addr_file(home: &Path, addr: SocketAddr) -> anyhow::Result<()> {
    write_atomic(home, ADDR_FILE, format!("{addr}\n").as_bytes())
}

/// A wildcard bind address is not connectable; use the loopback of the same family.
pub fn connect_addr(addr: SocketAddr) -> SocketAddr {
    if addr.ip().is_unspecified() {
        let ip: IpAddr = match addr.ip() {
            IpAddr::V4(_) => "127.0.0.1".parse().expect("literal"),
            IpAddr::V6(_) => "::1".parse().expect("literal"),
        };
        SocketAddr::new(ip, addr.port())
    } else {
        addr
    }
}

/// The connectable address of the local daemon from `<home>/daemon.addr`; `None` when the
/// file is missing or does not hold `host:port`.
pub fn local_addr(home: &Path) -> Option<SocketAddr> {
    std::fs::read_to_string(home.join(ADDR_FILE))
        .ok()?
        .trim()
        .parse()
        .ok()
        .map(connect_addr)
}

/// `owl daemon --foreground`: serve until the listener stops (or the process is killed).
pub async fn run_foreground(home: &Path, config: Config) -> anyhow::Result<()> {
    let running = spawn(home, config).await?;
    write_addr_file(home, running.addr)?;
    tracing::info!(path = %home.join(ADDR_FILE).display(), "wrote daemon.addr");
    running.wait().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addr_file_is_written_atomically() {
        let home = tempfile::tempdir().unwrap();
        let addr: SocketAddr = "127.0.0.1:4242".parse().unwrap();
        write_addr_file(home.path(), addr).unwrap();
        assert_eq!(
            std::fs::read_to_string(home.path().join(ADDR_FILE)).unwrap(),
            "127.0.0.1:4242\n"
        );
        assert!(!home.path().join("daemon.addr.tmp").exists());
        // Overwrite on restart.
        write_addr_file(home.path(), "127.0.0.1:1".parse().unwrap()).unwrap();
        assert_eq!(
            std::fs::read_to_string(home.path().join(ADDR_FILE)).unwrap(),
            "127.0.0.1:1\n"
        );
    }

    #[test]
    fn addr_file_failure_leaves_no_tmp() {
        let home = tempfile::tempdir().unwrap();
        // Destination blocked by a non-empty directory: rename must fail.
        let blocker = home.path().join(ADDR_FILE);
        std::fs::create_dir_all(blocker.join("child")).unwrap();
        let err = write_addr_file(home.path(), "127.0.0.1:1".parse().unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("renaming to"), "{err}");
        assert!(!home.path().join("daemon.addr.tmp").exists());
        assert!(blocker.is_dir(), "blocker untouched");
        // Missing home: the temp write itself fails.
        let err = write_addr_file(&home.path().join("missing"), "127.0.0.1:1".parse().unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("writing"), "{err}");
    }

    #[test]
    fn allowed_keys_skips_bad_pubkeys_and_adds_the_owner() {
        use crate::contacts::Contact;
        let good = Identity::from_seed([5u8; 32]);
        let owner = Identity::from_seed([6u8; 32]);
        let mk = |pubkey: &str| Contact {
            name: String::new(),
            emails: vec![],
            pubkey: pubkey.into(),
            endpoints: vec![],
            source: "local".into(),
            policy: None,
            added_at: None,
            fingerprint: String::new(),
        };
        let book = ContactBook {
            contacts: vec![
                mk(&identity::pubkey_string(&good.verifying_key())),
                mk("ed25519:AAAA"),
                mk("garbage"),
            ],
        };
        let keys = allowed_keys(&book, &owner);
        assert_eq!(keys.len(), 2);
        assert!(keys.contains(good.verifying_key().as_bytes()));
        assert!(keys.contains(owner.verifying_key().as_bytes()));
        let only_owner = allowed_keys(&ContactBook::default(), &owner);
        assert_eq!(only_owner.len(), 1);
        assert!(only_owner.contains(owner.verifying_key().as_bytes()));
    }

    #[test]
    fn local_addr_reads_daemon_addr_and_maps_wildcards() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(local_addr(home.path()), None, "no file");
        std::fs::write(home.path().join(ADDR_FILE), "garbage\n").unwrap();
        assert_eq!(local_addr(home.path()), None, "not host:port");
        write_addr_file(home.path(), "0.0.0.0:7411".parse().unwrap()).unwrap();
        assert_eq!(
            local_addr(home.path()),
            Some("127.0.0.1:7411".parse().unwrap())
        );
        write_addr_file(home.path(), "[::]:7411".parse().unwrap()).unwrap();
        assert_eq!(local_addr(home.path()), Some("[::1]:7411".parse().unwrap()));
        write_addr_file(home.path(), "10.0.0.5:1".parse().unwrap()).unwrap();
        assert_eq!(local_addr(home.path()), Some("10.0.0.5:1".parse().unwrap()));
    }

    /// AC1: the iroh endpoint id is the identity's public key, byte for byte, and the card
    /// repeats it as `iroh.id` next to `owlpost.pubkey`.
    #[tokio::test]
    async fn iroh_endpoint_id_is_identity_pubkey() {
        let home = tempfile::tempdir().unwrap();
        let id = Identity::from_seed([7u8; 32]);
        id.save(home.path()).unwrap();
        let cfg = Config {
            listen: "127.0.0.1:0".into(),
            relay_urls: Some(vec![]),
            ..Default::default()
        };
        let running = spawn(home.path(), cfg).await.unwrap();
        assert_eq!(
            running.iroh().id().as_bytes(),
            id.verifying_key().as_bytes()
        );
        assert_eq!(
            running.state.iroh.get().unwrap().id().as_bytes(),
            id.verifying_key().as_bytes()
        );
        let card = crate::server::card_json(&running.state);
        let pubkey = identity::pubkey_string(&id.verifying_key());
        // OWL-034: the bound endpoint shows as the `owl-iroh://<key>` interface (the key
        // without its `ed25519:` prefix), the relay (none here) in the identity extension.
        assert_eq!(
            card["supportedInterfaces"][1]["url"],
            format!("owl-iroh://{}", pubkey.strip_prefix("ed25519:").unwrap())
        );
        let params = crate::server::extension_params(&card, crate::server::EXT_IDENTITY).unwrap();
        assert_eq!(params["pubkey"], pubkey);
        assert_eq!(params["relay"], serde_json::Value::Null);
        // A different seed is a different id: the equality above is not vacuous.
        assert_ne!(
            running.iroh().id().as_bytes(),
            Identity::from_seed([8u8; 32]).verifying_key().as_bytes()
        );
        running.shutdown();
        running.wait().await.unwrap();
    }

    #[tokio::test]
    async fn spawn_rejects_bad_listen_and_missing_key() {
        let home = tempfile::tempdir().unwrap();
        let cfg = Config {
            listen: "127.0.0.1:0".into(),
            ..Default::default()
        };
        let err = spawn(home.path(), cfg.clone())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("reading"), "no key: {err}");
        Identity::from_seed([6u8; 32]).save(home.path()).unwrap();
        let bad = Config {
            listen: "nonsense".into(),
            ..cfg.clone()
        };
        let err = spawn(home.path(), bad).await.unwrap_err().to_string();
        assert!(err.contains("not host:port"), "{err}");
        // Port already taken → bind error surfaces, no panic.
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let busy = Config {
            listen: taken.local_addr().unwrap().to_string(),
            ..cfg
        };
        let err = format!("{:#}", spawn(home.path(), busy).await.unwrap_err());
        assert!(err.contains("binding 127.0.0.1:"), "{err}");
    }
}
