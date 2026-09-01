//! mTLS from the ed25519 identity: self-signed cert via rcgen, pinned verifiers (§4, §7).
//!
//! Crypto provider: `ring` for both rustls and rcgen — one backend, supports Ed25519
//! certificates and TLS 1.3 Ed25519 handshake signatures, accepts PKCS#8 v1 seeds, and
//! needs no cmake/nasm at build time (aws-lc-rs does). TLS 1.3 only (`tls12` feature off):
//! every peer is this same binary, so there is nothing to be compatible with, and it keeps
//! the verifier surface (and this spike) smaller.
//!
//! rustls `danger` APIs used, and why:
//! - `rustls::server::danger::ClientCertVerifier` — replaces the PKI chain check with
//!   "client SPKI key ∈ allowed set"; signatures are still verified via the provider.
//! - `rustls::client::danger::ServerCertVerifier` +
//!   `ClientConfig::dangerous().with_custom_certificate_verifier` — pins the server's
//!   SPKI key instead of checking a CA chain / hostname. `expected = None` accepts any
//!   server key and must only be used by `owl add` (TOFU).

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Context;
use ed25519_dalek::VerifyingKey;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, Error, ServerConfig, SignatureScheme,
};

use crate::identity::{self, Identity};

/// Raw ed25519 public keys allowed to connect. OWL-006 builds this from the ContactBook.
pub type AllowedKeys = HashSet<[u8; 32]>;

/// PKCS#8 v1 PrivateKeyInfo header for an Ed25519 seed (RFC 8410 §7).
const PKCS8_ED25519_HEADER: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];
/// SubjectPublicKeyInfo header for Ed25519: SEQ(42) { SEQ(5) { OID 1.3.101.112 }, BIT STRING(33) 0x00 }.
const SPKI_ED25519_HEADER: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Self-signed Ed25519 X.509 cert (CN = fingerprint) + PKCS#8 key for the identity.
pub fn cert_from_identity(
    id: &Identity,
) -> anyhow::Result<(CertificateDer<'static>, PrivateKeyDer<'static>)> {
    let mut pkcs8 = PKCS8_ED25519_HEADER.to_vec();
    pkcs8.extend_from_slice(&id.seed());
    let key_der = PrivatePkcs8KeyDer::from(pkcs8);
    let kp = rcgen::KeyPair::from_pkcs8_der_and_sign_algo(&key_der, &rcgen::PKCS_ED25519)
        .context("rcgen keypair from identity")?;
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new())?;
    params.distinguished_name.push(
        rcgen::DnType::CommonName,
        identity::fingerprint(&id.verifying_key()),
    );
    let cert = params
        .self_signed(&kp)
        .context("self-signing certificate")?;
    Ok((cert.der().clone(), PrivateKeyDer::Pkcs8(key_der)))
}

/// Raw 32-byte Ed25519 public key from the certificate's SubjectPublicKeyInfo.
// ponytail: byte-pattern scan for the Ed25519 SPKI — swap for x509-parser if certs get complex.
pub fn spki_pubkey(cert: &CertificateDer<'_>) -> anyhow::Result<[u8; 32]> {
    let der = cert.as_ref();
    let at = der
        .windows(SPKI_ED25519_HEADER.len())
        .position(|w| w == SPKI_ED25519_HEADER)
        .context("certificate has no Ed25519 SubjectPublicKeyInfo")?;
    let start = at + SPKI_ED25519_HEADER.len();
    der.get(start..start + 32)
        .and_then(|b| b.try_into().ok())
        .context("truncated SubjectPublicKeyInfo")
}

pub fn peer_fingerprint_from_cert(cert: &CertificateDer<'_>) -> anyhow::Result<String> {
    let key = VerifyingKey::from_bytes(&spki_pubkey(cert)?).context("invalid ed25519 key")?;
    Ok(identity::fingerprint(&key))
}

fn rejected() -> Error {
    Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure)
}

#[derive(Debug)]
struct PinnedClientVerifier {
    allowed: AllowedKeys,
    allow_unpinned: bool,
    provider: Arc<CryptoProvider>,
}

impl ClientCertVerifier for PinnedClientVerifier {
    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &[]
    }
    fn offer_client_auth(&self) -> bool {
        true
    }
    fn client_auth_mandatory(&self) -> bool {
        !self.allow_unpinned
    }
    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, Error> {
        match spki_pubkey(end_entity) {
            Ok(k) if self.allowed.contains(&k) => Ok(ClientCertVerified::assertion()),
            _ => Err(rejected()),
        }
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[derive(Debug)]
struct PinnedServerVerifier {
    expected: Option<[u8; 32]>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedServerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let key = spki_pubkey(end_entity).map_err(|_| rejected())?;
        match self.expected {
            Some(e) if e != key => Err(rejected()),
            _ => Ok(ServerCertVerified::assertion()),
        }
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Server accepting clients whose cert key is in `allowed`; with `allow_unpinned_for_card`
/// also clients presenting no certificate (card routes only — handlers must check).
pub fn server_config(
    id: &Identity,
    allowed: AllowedKeys,
    allow_unpinned_for_card: bool,
) -> anyhow::Result<Arc<ServerConfig>> {
    let (cert, key) = cert_from_identity(id)?;
    let provider = provider();
    let verifier = PinnedClientVerifier {
        allowed,
        allow_unpinned: allow_unpinned_for_card,
        provider: provider.clone(),
    };
    let cfg = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_client_cert_verifier(Arc::new(verifier))
        .with_single_cert(vec![cert], key)?;
    Ok(Arc::new(cfg))
}

/// Client pinned to `expected` server key (`None` = TOFU, `owl add` only). `id = None`
/// presents no certificate (card fetch from a peer that does not know us yet).
pub fn client_config(
    id: Option<&Identity>,
    expected: Option<[u8; 32]>,
) -> anyhow::Result<Arc<ClientConfig>> {
    let provider = provider();
    let verifier = PinnedServerVerifier {
        expected,
        provider: provider.clone(),
    };
    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier));
    let cfg = match id {
        Some(id) => {
            let (cert, key) = cert_from_identity(id)?;
            builder.with_client_auth_cert(vec![cert], key)?
        }
        None => builder.with_no_client_auth(),
    };
    Ok(Arc::new(cfg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cert_carries_identity_key() {
        let id = Identity::from_seed([9u8; 32]);
        let (cert, _) = cert_from_identity(&id).unwrap();
        assert_eq!(spki_pubkey(&cert).unwrap(), *id.verifying_key().as_bytes());
    }

    #[test]
    fn fingerprint_from_cert_matches_identity() {
        let id = Identity::from_seed([10u8; 32]);
        let (cert, _) = cert_from_identity(&id).unwrap();
        assert_eq!(
            peer_fingerprint_from_cert(&cert).unwrap(),
            identity::fingerprint(&id.verifying_key())
        );
    }

    #[test]
    fn spki_rejects_non_ed25519_der() {
        let no_spki = "certificate has no Ed25519 SubjectPublicKeyInfo";
        let junk = CertificateDer::from(vec![0x30, 0x03, 0x02, 0x01, 0x05]);
        assert_eq!(spki_pubkey(&junk).unwrap_err().to_string(), no_spki);
        // X25519 SPKI (OID 1.3.101.110) with a 32-byte key: right shape, wrong algorithm.
        let mut x25519 = SPKI_ED25519_HEADER.to_vec();
        x25519[8] = 0x6e;
        x25519.extend_from_slice(&[1u8; 32]);
        assert_eq!(
            spki_pubkey(&CertificateDer::from(x25519))
                .unwrap_err()
                .to_string(),
            no_spki
        );
        let mut truncated = SPKI_ED25519_HEADER.to_vec();
        truncated.extend_from_slice(&[1u8; 31]);
        assert_eq!(
            spki_pubkey(&CertificateDer::from(truncated))
                .unwrap_err()
                .to_string(),
            "truncated SubjectPublicKeyInfo"
        );
    }
}
