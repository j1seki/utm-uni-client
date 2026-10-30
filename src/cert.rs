//! Client certificate handling, TLS configuration and fingerprint helpers.
//!
//! UTM Remote uses mutual TLS.  The client certificate can be self-signed.
//! Trust is established by comparing a *connection fingerprint*: the SHA-256
//! of the server certificate XORed byte-wise with the SHA-256 of the client
//! certificate.  This is exactly what pyutmremote does.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose, PKCS_RSA_SHA256,
};
use rsa::pkcs8::EncodePrivateKey;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{ring, CryptoProvider};
use rustls::pki_types::{
    CertificateDer, PrivateKeyDer, PrivatePkcs1KeyDer, PrivatePkcs8KeyDer, PrivateSec1KeyDer,
    ServerName, UnixTime,
};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

pub const DEFAULT_CERT_PASSWORD: &str = "password";

#[derive(Clone)]
pub struct ClientIdentity {
    pub cert_der: CertificateDer<'static>,
    pub key_der: Arc<PrivateKeyDer<'static>>,
    pub fingerprint: [u8; 32],
    pub path: PathBuf,
}

impl std::fmt::Debug for ClientIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientIdentity")
            .field("path", &self.path)
            .field("fingerprint", &format_fingerprint(&self.fingerprint))
            .finish()
    }
}

impl ClientIdentity {
    pub fn fingerprint_string(&self) -> String {
        format_fingerprint(&self.fingerprint)
    }
}

/// Load a pyutmremote-compatible certificate file, generating one if missing.
pub fn load_or_generate(path: &Path, password: &str) -> Result<ClientIdentity> {
    if path.exists() {
        load_identity(path, password)
    } else {
        generate_identity(path)
    }
}

pub fn load_identity(path: &Path, password: &str) -> Result<ClientIdentity> {
    let text = fs::read_to_string(path)
        .map_err(|e| Error::Config(format!("cannot read client certificate {path:?}: {e}")))?;
    let blocks = pem_blocks(&text)?;
    let mut certs = Vec::new();
    let mut key: Option<PrivateKeyDer<'static>> = None;

    for (label, der) in blocks {
        match label.as_str() {
            "CERTIFICATE" => certs.push(CertificateDer::from(der)),
            "PRIVATE KEY" => {
                key = Some(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(der)));
            }
            "RSA PRIVATE KEY" => {
                key = Some(PrivateKeyDer::Pkcs1(PrivatePkcs1KeyDer::from(der)));
            }
            "EC PRIVATE KEY" => {
                key = Some(PrivateKeyDer::Sec1(PrivateSec1KeyDer::from(der)));
            }
            "ENCRYPTED PRIVATE KEY" => {
                let decrypted = decrypt_pkcs8(&der, password)?;
                key = Some(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(decrypted)));
            }
            other => {
                return Err(Error::Config(format!(
                    "unsupported PEM block in {path:?}: {other}"
                )))
            }
        }
    }

    let cert = certs
        .first()
        .cloned()
        .ok_or_else(|| Error::Config(format!("no certificate in {path:?}")))?;
    let key = key.ok_or_else(|| Error::Config(format!("no private key in {path:?}")))?;
    let fingerprint = fingerprint_der(cert.as_ref());
    Ok(ClientIdentity {
        cert_der: cert,
        key_der: Arc::new(key),
        fingerprint,
        path: path.to_path_buf(),
    })
}

/// Generate an RSA-4096 self-signed client certificate, matching the
/// pyutmremote certificate profile (O=UTM, CN=UTM Remote Client).
pub fn generate_identity(path: &Path) -> Result<ClientIdentity> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| Error::Config(format!("cannot create {parent:?}: {e}")))?;
    }

    let mut rng = rand::thread_rng();
    let rsa_key = rsa::RsaPrivateKey::new(&mut rng, 4096)
        .map_err(|e| Error::Config(format!("RSA key generation failed: {e}")))?;
    let pkcs8 = rsa_key
        .to_pkcs8_der()
        .map_err(|e| Error::Config(format!("PKCS#8 encoding failed: {e}")))?;
    let pkcs8_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(pkcs8.as_bytes().to_vec()));
    let key_pair = KeyPair::from_der_and_sign_algo(&pkcs8_der, &PKCS_RSA_SHA256)
        .map_err(|e| Error::Config(format!("cannot create rcgen key pair: {e}")))?;

    let mut params = CertificateParams::new(Vec::<String>::new())
        .map_err(|e| Error::Config(format!("certificate params: {e}")))?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::OrganizationName, "UTM");
    dn.push(DnType::CommonName, "UTM Remote Client");
    params.distinguished_name = dn;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
    ];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    params.not_before = rcgen::date_time_ymd(2020, 1, 1);
    params.not_after = rcgen::date_time_ymd(2035, 1, 1);

    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| Error::Config(format!("certificate signing failed: {e}")))?;
    let cert_pem = cert.pem();
    let key_pem = key_pair.serialize_pem();

    // Match pyutmremote's layout: private key first, then certificate.
    let combined = format!("{key_pem}{cert_pem}");
    write_private_file(path, combined.as_bytes())?;

    let cert_der = cert.der().clone();
    let key_der = key_pair.serialize_der();
    let fingerprint = fingerprint_der(cert_der.as_ref());
    Ok(ClientIdentity {
        cert_der,
        key_der: Arc::new(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der))),
        fingerprint,
        path: path.to_path_buf(),
    })
}

fn write_private_file(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut options = fs::OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&tmp)
            .map_err(|e| Error::Config(format!("cannot create {tmp:?}: {e}")))?;
        file.write_all(data)
            .map_err(|e| Error::Config(format!("cannot write {tmp:?}: {e}")))?;
        file.sync_all().ok();
    }
    fs::rename(&tmp, path)
        .map_err(|e| Error::Config(format!("cannot rename {tmp:?} to {path:?}: {e}")))?;
    Ok(())
}

fn decrypt_pkcs8(der: &[u8], password: &str) -> Result<Vec<u8>> {
    use pkcs8::der::Decode;
    let info = pkcs8::EncryptedPrivateKeyInfo::from_der(der)
        .map_err(|e| Error::Config(format!("invalid encrypted PKCS#8: {e}")))?;
    let decrypted = info
        .decrypt(password.as_bytes())
        .map_err(|e| Error::Config(format!("cannot decrypt client key: {e}")))?;
    Ok(decrypted.as_bytes().to_vec())
}

fn pem_blocks(text: &str) -> Result<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(begin) = rest.find("-----BEGIN ") {
        let after = &rest[begin + 11..];
        let label_end = after
            .find("-----")
            .ok_or_else(|| Error::Config("malformed PEM header".into()))?;
        let label = after[..label_end].to_owned();
        let body_start = begin + 11 + label_end + 5;
        let end_marker = format!("-----END {label}-----");
        let body_end = rest[body_start..]
            .find(&end_marker)
            .ok_or_else(|| Error::Config(format!("missing PEM footer for {label}")))?
            + body_start;
        let b64: String = rest[body_start..body_end]
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let der = base64::engine::general_purpose::STANDARD
            .decode(b64.as_bytes())
            .map_err(|e| Error::Config(format!("invalid base64 in PEM block: {e}")))?;
        out.push((label, der));
        rest = &rest[body_end + end_marker.len()..];
    }
    if out.is_empty() {
        return Err(Error::Config("no PEM blocks found".into()));
    }
    Ok(out)
}

pub fn fingerprint_der(der: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(der);
    hasher.finalize().into()
}

pub fn format_fingerprint(fingerprint: &[u8; 32]) -> String {
    fingerprint
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

pub fn parse_fingerprint(text: &str) -> Result<[u8; 32]> {
    let cleaned: String = text.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if cleaned.len() != 64 {
        return Err(Error::Config(format!(
            "fingerprint must contain 64 hex digits, got {}",
            cleaned.len()
        )));
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&cleaned[i * 2..i * 2 + 2], 16)
            .map_err(|e| Error::Config(format!("invalid fingerprint: {e}")))?;
    }
    Ok(out)
}

pub fn connection_fingerprint(server: &[u8; 32], client: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = server[i] ^ client[i];
    }
    out
}

pub fn server_certificate_fingerprint(
    tls: &tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
) -> Result<[u8; 32]> {
    let (_, session) = tls.get_ref();
    let certs = session
        .peer_certificates()
        .ok_or_else(|| Error::Tls(rustls::Error::NoCertificatesPresented))?;
    let cert = certs
        .first()
        .ok_or_else(|| Error::Tls(rustls::Error::NoCertificatesPresented))?;
    Ok(fingerprint_der(cert.as_ref()))
}

pub fn utm_client_config(identity: &ClientIdentity) -> Result<ClientConfig> {
    let provider = Arc::new(ring::default_provider());
    let verifier = Arc::new(AcceptAnyVerifier {
        provider: Arc::clone(&provider),
    });
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(Error::Tls)?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(
            vec![identity.cert_der.clone()],
            identity.key_der.as_ref().clone_key(),
        )
        .map_err(Error::Tls)?;
    Ok(config)
}

/// SPICE TLS config.  The server certificate's SubjectPublicKeyInfo must match
/// the `spicePublicKey` returned by the UTM server.
pub fn spice_client_config(expected_spki: Vec<u8>) -> Result<ClientConfig> {
    let provider = Arc::new(ring::default_provider());
    let verifier = Arc::new(SpiceVerifier {
        provider: Arc::clone(&provider),
        expected_spki,
    });
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(Error::Tls)?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    Ok(config)
}

#[derive(Debug)]
struct AcceptAnyVerifier {
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for AcceptAnyVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        // UTM uses self-signed certificates and pins the connection
        // fingerprint at the application layer, exactly like pyutmremote's
        // ssl.CERT_NONE context.
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
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
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
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
struct SpiceVerifier {
    provider: Arc<CryptoProvider>,
    expected_spki: Vec<u8>,
}

impl ServerCertVerifier for SpiceVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        let spki = extract_spki(end_entity.as_ref())
            .map_err(|e| rustls::Error::General(format!("invalid SPICE certificate: {e}")))?;
        if spki != self.expected_spki {
            return Err(rustls::Error::General(
                "SPICE certificate public key does not match server information".into(),
            ));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
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
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
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

/// Extract the DER-encoded SubjectPublicKeyInfo from an X.509 certificate.
/// The UTM server exposes the SPKI directly in `spicePublicKey`.
pub fn extract_spki(cert: &[u8]) -> Result<Vec<u8>> {
    let cert = DerCursor::new(cert);
    let (tag, cert_body) = cert.read_tlv(0)?;
    if tag != 0x30 {
        return Err(Error::Config("certificate is not a SEQUENCE".into()));
    }
    let mut pos = cert_body.0;
    let (tag, tbs) = cert.read_tlv(pos)?;
    if tag != 0x30 {
        return Err(Error::Config("tbsCertificate is not a SEQUENCE".into()));
    }
    pos = tbs.0;
    // [0] EXPLICIT version (optional)
    let (tag, _) = cert.read_tlv(pos)?;
    if tag == 0xa0 {
        pos = cert.tlv_end(pos)?;
    }
    // serialNumber
    let (tag, _) = cert.read_tlv(pos)?;
    if tag != 0x02 {
        return Err(Error::Config("serialNumber missing".into()));
    }
    pos = cert.tlv_end(pos)?;
    // signature AlgorithmIdentifier
    pos = cert.tlv_end(pos)?;
    // issuer
    pos = cert.tlv_end(pos)?;
    // validity
    pos = cert.tlv_end(pos)?;
    // subject
    pos = cert.tlv_end(pos)?;
    // subjectPublicKeyInfo
    let start = pos;
    let (tag, _) = cert.read_tlv(pos)?;
    if tag != 0x30 {
        return Err(Error::Config("subjectPublicKeyInfo missing".into()));
    }
    let end = cert.tlv_end(pos)?;
    if end > end_of(cert_body) {
        return Err(Error::Config("truncated certificate".into()));
    }
    Ok(cert.bytes[start..end].to_vec())
}

fn end_of((_, end): (usize, usize)) -> usize {
    end
}

struct DerCursor<'a> {
    bytes: &'a [u8],
}

impl<'a> DerCursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    fn read_tlv(&self, pos: usize) -> Result<(u8, (usize, usize))> {
        let tag = *self
            .bytes
            .get(pos)
            .ok_or_else(|| Error::Config("truncated DER tag".into()))?;
        let first = *self
            .bytes
            .get(pos + 1)
            .ok_or_else(|| Error::Config("truncated DER length".into()))?;
        let (len, header) = if first & 0x80 == 0 {
            (first as usize, 2usize)
        } else {
            let count = (first & 0x7f) as usize;
            if count == 0 || count > 8 {
                return Err(Error::Config("unsupported DER length".into()));
            }
            let mut len = 0usize;
            for i in 0..count {
                let b = *self
                    .bytes
                    .get(pos + 2 + i)
                    .ok_or_else(|| Error::Config("truncated DER length".into()))?;
                len = (len << 8) | b as usize;
            }
            (len, 2 + count)
        };
        let start = pos + header;
        let end = start
            .checked_add(len)
            .ok_or_else(|| Error::Config("DER length overflow".into()))?;
        if end > self.bytes.len() {
            return Err(Error::Config("truncated DER value".into()));
        }
        Ok((tag, (start, end)))
    }

    fn tlv_end(&self, pos: usize) -> Result<usize> {
        let (_, (_, end)) = self.read_tlv(pos)?;
        Ok(end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_roundtrip() {
        let fp = [0x12u8; 32];
        let text = format_fingerprint(&fp);
        assert_eq!(parse_fingerprint(&text).unwrap(), fp);
    }

    #[test]
    fn connection_fingerprint_is_xor() {
        let a = [0xAAu8; 32];
        let b = [0x55u8; 32];
        assert_eq!(connection_fingerprint(&a, &b), [0xFFu8; 32]);
    }

    #[test]
    fn generated_certificate_has_spki() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client.pem");
        let identity = generate_identity(&path).unwrap();
        let spki = extract_spki(identity.cert_der.as_ref()).unwrap();
        assert!(!spki.is_empty());
        let loaded = load_identity(&path, DEFAULT_CERT_PASSWORD).unwrap();
        assert_eq!(loaded.fingerprint, identity.fingerprint);
    }
}
