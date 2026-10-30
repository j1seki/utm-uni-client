//! Compatibility tests against a mock UTM Remote server.
//!
//! The mock speaks the same framing and Codable payloads as UTM Remote and
//! requires a client certificate, so it exercises the real TLS, fingerprint
//! and handshake paths without needing a Mac.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rcgen::{generate_simple_self_signed, CertifiedKey};
use rustls::client::danger::HandshakeSignatureValid;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::DistinguishedName;
use rustls::{DigitallySignedStruct, SignatureScheme};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;

use utm_web::cert::{connection_fingerprint, fingerprint_der, format_fingerprint, ClientIdentity};
use utm_web::codable::{decode, encode, Type, Value};
use utm_web::error::Error;
use utm_web::utm::{UtmClient, UtmConnectOptions};

#[derive(Debug)]
struct AcceptAnyClientCert;

impl ClientCertVerifier for AcceptAnyClientCert {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn self_signed() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let CertifiedKey { cert, key_pair } =
        generate_simple_self_signed(vec!["localhost".into()]).expect("generate certificate");
    let key = PrivatePkcs8KeyDer::from(key_pair.serialize_der());
    (cert.der().clone(), PrivateKeyDer::Pkcs8(key))
}

fn client_identity() -> (ClientIdentity, CertificateDer<'static>) {
    let (cert, key) = self_signed();
    let fingerprint = fingerprint_der(cert.as_ref());
    (
        ClientIdentity {
            cert_der: cert.clone(),
            key_der: Arc::new(key),
            fingerprint,
            path: PathBuf::from("mock-client.pem"),
        },
        cert,
    )
}

fn value_bool(value: bool) -> Value {
    Value::Bool(value)
}

fn handshake_reply(authenticated: bool) -> Value {
    Value::dict([
        ("version", Value::Int(1)),
        ("isAuthenticated", value_bool(authenticated)),
        ("capabilities", Value::dict([("rawValue", Value::Int(0))])),
        ("model", Value::Str("Mock Mac".into())),
    ])
}

fn vm_info() -> Value {
    Value::dict([
        ("id", Value::Str("vm-1".into())),
        ("name", Value::Str("Test VM".into())),
        ("path", Value::Str("/tmp/test.utm".into())),
        ("isShortcut", Value::Bool(false)),
        ("isSuspended", Value::Bool(false)),
        ("isTakeoverAllowed", Value::Bool(true)),
        // Real UTM sends a String raw value for `UTMBackend`.
        ("backend", Value::Str("QEMU".into())),
        // ...and an Int raw value for `UTMVirtualMachineState`.
        ("state", Value::Int(2)),
        ("mountedDrives", Value::Dict(Default::default())),
    ])
}

async fn read_frame(
    stream: &mut tokio_rustls::server::TlsStream<TcpStream>,
) -> std::io::Result<Vec<u8>> {
    let mut header = [0u8; 8];
    stream.read_exact(&mut header).await?;
    let length = u64::from_be_bytes(header) as usize;
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).await?;
    Ok(body)
}

async fn write_frame(
    stream: &mut tokio_rustls::server::TlsStream<TcpStream>,
    payload: &[u8],
) -> std::io::Result<()> {
    stream
        .write_all(&(payload.len() as u64).to_be_bytes())
        .await?;
    stream.write_all(payload).await?;
    stream.flush().await
}

fn read_uleb(data: &[u8], pos: &mut usize) -> u64 {
    let mut value = 0u64;
    let mut shift = 0;
    loop {
        let byte = data[*pos];
        *pos += 1;
        value |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return value;
        }
        shift += 7;
    }
}

fn frame(id: u8, flags: u8, token: u64, body: &[u8]) -> Vec<u8> {
    let mut out = vec![id, flags];
    let mut value = token;
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
    out.extend_from_slice(body);
    out
}

struct MockServer {
    addr: std::net::SocketAddr,
    server_cert: CertificateDer<'static>,
    seen_password: Arc<Mutex<Option<String>>>,
}

async fn start_mock(authenticated: bool) -> MockServer {
    let (server_cert, server_key) = self_signed();
    let config = rustls::ServerConfig::builder()
        .with_client_cert_verifier(Arc::new(AcceptAnyClientCert))
        .with_single_cert(vec![server_cert.clone()], server_key)
        .expect("server config");
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let seen_password = Arc::new(Mutex::new(None));

    let password_slot = Arc::clone(&seen_password);
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                break;
            };
            let acceptor = acceptor.clone();
            let password_slot = Arc::clone(&password_slot);
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(tcp).await else {
                    return;
                };
                loop {
                    let Ok(payload) = read_frame(&mut tls).await else {
                        return;
                    };
                    if payload.len() < 3 {
                        return;
                    }
                    let id = payload[0];
                    let flags = payload[1];
                    let mut pos = 2;
                    let token = read_uleb(&payload, &mut pos);
                    if flags & 1 != 0 {
                        continue;
                    }
                    let body = &payload[pos..];
                    let reply = match id {
                        0 => {
                            let request = decode(
                                body,
                                Type::Struct(&[("version", Type::Int), ("password", Type::Str)]),
                            )
                            .expect("client handshake");
                            if let Some(Value::Str(password)) = request.get("password") {
                                *password_slot.lock().unwrap() = Some(password.clone());
                            }
                            handshake_reply(authenticated)
                        }
                        1 => Value::dict([("ids", Value::Array(vec![Value::Str("vm-1".into())]))]),
                        3 => Value::dict([("informations", Value::Array(vec![vm_info()]))]),
                        _ => Value::empty(),
                    };
                    let encoded = encode(&reply).expect("encode reply");
                    if write_frame(&mut tls, &frame(id, 1, token, &encoded))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
    });

    MockServer {
        addr,
        server_cert,
        seen_password,
    }
}

fn options(
    addr: std::net::SocketAddr,
    expected_fingerprint: Option<[u8; 32]>,
    allow_untrusted: bool,
) -> UtmConnectOptions {
    UtmConnectOptions {
        host: "localhost".into(),
        port: addr.port(),
        password: Some("secret".into()),
        expected_fingerprint,
        allow_untrusted,
    }
}

fn expected_fingerprint(
    server: &CertificateDer<'static>,
    client: &CertificateDer<'static>,
) -> [u8; 32] {
    connection_fingerprint(
        &fingerprint_der(server.as_ref()),
        &fingerprint_der(client.as_ref()),
    )
}

#[tokio::test]
async fn handshake_fingerprint_and_vm_listing() {
    let server = start_mock(true).await;
    let (identity, client_cert) = client_identity();
    let expected = expected_fingerprint(&server.server_cert, &client_cert);

    let (client, _events) = UtmClient::connect(
        Arc::new(identity),
        options(server.addr, Some(expected), false),
    )
    .await
    .expect("connect");

    assert_eq!(client.model, "Mock Mac");
    assert_eq!(client.connection_fingerprint, expected);
    assert_eq!(
        client.connection_fingerprint_string(),
        format_fingerprint(&expected)
    );

    let vms = client.list_vms().await.expect("list vms");
    assert_eq!(vms.len(), 1);
    assert_eq!(vms[0].id, "vm-1");
    assert_eq!(vms[0].name, "Test VM");
    assert_eq!(vms[0].backend, "qemu");

    let seen = server.seen_password.lock().unwrap().clone();
    assert_eq!(seen.as_deref(), Some("secret"));
}

#[tokio::test]
async fn untrusted_server_reports_connection_fingerprint() {
    let server = start_mock(true).await;
    let (identity, client_cert) = client_identity();
    let expected = expected_fingerprint(&server.server_cert, &client_cert);

    let error = UtmClient::connect(Arc::new(identity), options(server.addr, None, false))
        .await
        .expect_err("untrusted connection must be refused");

    match error {
        Error::Untrusted { fingerprint } => {
            assert_eq!(fingerprint, format_fingerprint(&expected));
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn fingerprint_mismatch_is_rejected() {
    let server = start_mock(true).await;
    let (identity, _) = client_identity();

    let error = UtmClient::connect(
        Arc::new(identity),
        options(server.addr, Some([0u8; 32]), false),
    )
    .await
    .expect_err("mismatched fingerprint must be refused");

    assert!(matches!(error, Error::FingerprintMismatch { .. }));
}

#[tokio::test]
async fn rejected_password_is_reported() {
    let server = start_mock(false).await;
    let (identity, client_cert) = client_identity();
    let expected = expected_fingerprint(&server.server_cert, &client_cert);

    let error = UtmClient::connect(
        Arc::new(identity),
        options(server.addr, Some(expected), false),
    )
    .await
    .expect_err("unauthenticated handshake must fail");

    assert!(matches!(error, Error::InvalidPassword));
}
