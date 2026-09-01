//! OWL-005: pinned mTLS handshakes over a real TCP socket (127.0.0.1:0, sync rustls).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use owlpost::identity::{Identity, fingerprint};
use owlpost::tls::{AllowedKeys, client_config, peer_fingerprint_from_cert, server_config};
use rustls::pki_types::ServerName;
use rustls::{ClientConnection, ServerConnection, StreamOwned};

const TIMEOUT: Duration = Duration::from_secs(3);

fn id(n: u8) -> Identity {
    Identity::from_seed([n; 32])
}
fn key(id: &Identity) -> [u8; 32] {
    *id.verifying_key().as_bytes()
}

/// Server thread: accept one connection, complete the handshake, write the client's
/// fingerprint (or "-" when no cert) back, return Ok(()) or the handshake error.
fn spawn_server(cfg: Arc<rustls::ServerConfig>) -> (u16, JoinHandle<Result<(), String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = thread::spawn(move || {
        let (tcp, _) = listener.accept().map_err(|e| e.to_string())?;
        tcp.set_read_timeout(Some(TIMEOUT)).unwrap();
        let conn = ServerConnection::new(cfg).map_err(|e| e.to_string())?;
        let mut s = StreamOwned::new(conn, tcp);
        s.conn.complete_io(&mut s.sock).map_err(|e| e.to_string())?;
        let fp = match s.conn.peer_certificates() {
            Some([cert, ..]) => peer_fingerprint_from_cert(cert).map_err(|e| e.to_string())?,
            _ => "-".to_string(),
        };
        s.write_all(fp.as_bytes()).map_err(|e| e.to_string())?;
        s.conn.send_close_notify();
        let _ = s.flush();
        Ok(())
    });
    (port, h)
}

/// Client: handshake, then read whatever the server sent. Err on handshake failure.
fn connect(port: u16, cfg: Arc<rustls::ClientConfig>) -> Result<String, String> {
    let tcp = TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
    tcp.set_read_timeout(Some(TIMEOUT)).unwrap();
    let name = ServerName::try_from("owlpost").unwrap();
    let conn = ClientConnection::new(cfg, name).map_err(|e| e.to_string())?;
    let mut s = StreamOwned::new(conn, tcp);
    s.conn.complete_io(&mut s.sock).map_err(|e| e.to_string())?;
    let mut buf = String::new();
    s.read_to_string(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

fn book(ids: &[&Identity]) -> AllowedKeys {
    ids.iter().map(|i| key(i)).collect()
}

#[test]
fn pinned_handshake_succeeds() {
    let (a, b) = (id(1), id(2));
    let (port, srv) = spawn_server(server_config(&b, book(&[&a]), false).unwrap());
    let got = connect(port, client_config(Some(&a), Some(key(&b))).unwrap()).unwrap();
    assert_eq!(got, fingerprint(&a.verifying_key()));
    srv.join().unwrap().unwrap();
}

#[test]
fn unknown_client_is_refused() {
    let (a, b, c) = (id(1), id(2), id(3));
    let (port, srv) = spawn_server(server_config(&b, book(&[&a]), false).unwrap());
    let res = connect(port, client_config(Some(&c), Some(key(&b))).unwrap());
    let server_err = srv.join().unwrap().unwrap_err();
    assert!(res.is_err(), "client should not get data: {res:?}");
    assert!(
        server_err.contains("invalid peer certificate"),
        "server: {server_err}"
    );
}

#[test]
fn wrong_server_key_is_refused() {
    let (a, b, c) = (id(1), id(2), id(3));
    let (port, srv) = spawn_server(server_config(&b, book(&[&a]), false).unwrap());
    let err = connect(port, client_config(Some(&a), Some(key(&c))).unwrap()).unwrap_err();
    assert!(err.contains("invalid peer certificate"), "client: {err}");
    assert!(srv.join().unwrap().is_err());
}

#[test]
fn unpinned_client_allowed_when_enabled() {
    let (a, b) = (id(1), id(2));
    let (port, srv) = spawn_server(server_config(&b, book(&[&a]), true).unwrap());
    let got = connect(port, client_config(None, Some(key(&b))).unwrap()).unwrap();
    assert_eq!(got, "-", "server must see no client certificate");
    srv.join().unwrap().unwrap();

    let (port, srv) = spawn_server(server_config(&b, book(&[&a]), false).unwrap());
    let res = connect(port, client_config(None, Some(key(&b))).unwrap());
    let server_err = srv.join().unwrap().unwrap_err();
    assert!(
        res.is_err(),
        "flag=false must refuse a cert-less client: {res:?}"
    );
    assert!(server_err.contains("certificate"), "server: {server_err}");
}

#[test]
fn tofu_client_accepts_any_server_key() {
    let (a, b) = (id(1), id(2));
    let (port, srv) = spawn_server(server_config(&b, book(&[&a]), false).unwrap());
    let got = connect(port, client_config(Some(&a), None).unwrap()).unwrap();
    assert_eq!(got, fingerprint(&a.verifying_key()));
    srv.join().unwrap().unwrap();
}
