//! Local mock UTM Remote server used for end-to-end testing of the web UI.
//!
//! This is not a real UTM server.  It speaks the same SwiftConnect framing and
//! Codable payloads, requires a client certificate, performs both handshakes,
//! serves a small VM list, and acknowledges VM actions with transition events.

use std::collections::BTreeMap;
use std::sync::Arc;

use rcgen::{generate_simple_self_signed, CertifiedKey};
use rustls::client::danger::HandshakeSignatureValid;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::DistinguishedName;
use rustls::{DigitallySignedStruct, SignatureScheme};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;

use utm_web::codable::{decode, encode, Type, Value};

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

fn vm_info(id: &str, name: &str, state: i64) -> Value {
    Value::dict([
        ("id", Value::Str(id.into())),
        ("name", Value::Str(name.into())),
        ("path", Value::Str(format!("/Users/mock/VMs/{name}.utm"))),
        ("isShortcut", Value::Bool(false)),
        ("isSuspended", Value::Bool(false)),
        ("isTakeoverAllowed", Value::Bool(true)),
        ("backend", Value::Str("QEMU".into())),
        ("state", Value::Int(state)),
        ("mountedDrives", Value::Dict(BTreeMap::new())),
    ])
}

fn read_uleb(data: &[u8], pos: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    let mut shift = 0u32;
    loop {
        let byte = *data.get(*pos)?;
        *pos += 1;
        if shift >= 64 {
            return None;
        }
        value |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift += 7;
    }
}

fn push_uleb(out: &mut Vec<u8>, mut value: u64) {
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
}

fn frame(id: u8, flags: u8, token: u64, body: &[u8]) -> Vec<u8> {
    let mut out = vec![id, flags];
    push_uleb(&mut out, token);
    out.extend_from_slice(body);
    out
}

async fn read_frame(stream: &mut tokio_rustls::server::TlsStream<TcpStream>) -> Option<Vec<u8>> {
    let mut header = [0u8; 8];
    stream.read_exact(&mut header).await.ok()?;
    let len = u64::from_be_bytes(header) as usize;
    if len > 8 * 1024 * 1024 {
        return None;
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).await.ok()?;
    Some(body)
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

async fn send_request(
    stream: &mut tokio_rustls::server::TlsStream<TcpStream>,
    id: u8,
    token: u64,
    body: &Value,
) -> std::io::Result<()> {
    let encoded = encode(body).map_err(std::io::Error::other)?;
    write_frame(stream, &frame(id, 0, token, &encoded)).await
}

async fn send_reply(
    stream: &mut tokio_rustls::server::TlsStream<TcpStream>,
    id: u8,
    token: u64,
    body: &Value,
) -> std::io::Result<()> {
    let encoded = encode(body).map_err(std::io::Error::other)?;
    write_frame(stream, &frame(id, 1, token, &encoded)).await
}

async fn transition(
    stream: &mut tokio_rustls::server::TlsStream<TcpStream>,
    token: u64,
    id: &str,
    state: i64,
) -> std::io::Result<()> {
    let body = Value::dict([
        ("id", Value::Str(id.into())),
        ("state", Value::Int(state)),
        ("isTakeoverAllowed", Value::Bool(true)),
    ]);
    send_request(stream, 4, token, &body).await
}

async fn handle_connection(mut tls: tokio_rustls::server::TlsStream<TcpStream>) {
    eprintln!("mock: client connected");
    let mut next_server_token = 1u64;
    let handshake_token = next_server_token;
    next_server_token += 1;
    if send_request(
        &mut tls,
        0,
        handshake_token,
        &Value::dict([("version", Value::Int(1))]),
    )
    .await
    .is_err()
    {
        return;
    }

    loop {
        let Some(payload) = read_frame(&mut tls).await else {
            break;
        };
        if payload.len() < 3 {
            break;
        }
        let id = payload[0];
        let flags = payload[1];
        let mut pos = 2;
        let Some(token) = read_uleb(&payload, &mut pos) else {
            break;
        };
        let body = &payload[pos..];

        if flags & 1 != 0 {
            if token == handshake_token {
                match decode(
                    body,
                    Type::Struct(&[
                        ("version", Type::Int),
                        ("capabilities", Type::Struct(&[("rawValue", Type::Int)])),
                    ]),
                ) {
                    Ok(reply) => eprintln!(
                        "mock: client handshake reply version={:?} capabilities={:?}",
                        reply.get("version").and_then(Value::as_i64),
                        reply
                            .get("capabilities")
                            .and_then(|v| v.get("rawValue"))
                            .and_then(Value::as_i64)
                    ),
                    Err(error) => eprintln!("mock: bad client handshake reply: {error}"),
                }
            }
            continue;
        }

        match id {
            0 => {
                let request = decode(
                    body,
                    Type::Struct(&[("version", Type::Int), ("password", Type::Str)]),
                );
                eprintln!(
                    "mock: server handshake request {:?}",
                    request.map(|v| v.get("version").and_then(Value::as_i64))
                );
                let reply = Value::dict([
                    ("version", Value::Int(1)),
                    ("isAuthenticated", Value::Bool(true)),
                    ("capabilities", Value::dict([("rawValue", Value::Int(13))])),
                    ("model", Value::Str("Mock Mac".into())),
                ]);
                let _ = send_reply(&mut tls, id, token, &reply).await;
            }
            1 => {
                eprintln!("mock: list virtual machines");
                let reply = Value::dict([(
                    "ids",
                    Value::Array(vec![
                        Value::Str("A1B2C3D4-0000-0000-0000-000000000001".into()),
                        Value::Str("A1B2C3D4-0000-0000-0000-000000000002".into()),
                    ]),
                )]);
                let _ = send_reply(&mut tls, id, token, &reply).await;
            }
            3 => {
                eprintln!("mock: get virtual machine information");
                let reply = Value::dict([(
                    "informations",
                    Value::Array(vec![
                        vm_info("A1B2C3D4-0000-0000-0000-000000000001", "Windows 11", 2),
                        vm_info("A1B2C3D4-0000-0000-0000-000000000002", "Ubuntu", 0),
                    ]),
                )]);
                let _ = send_reply(&mut tls, id, token, &reply).await;
            }
            10 => {
                let request = decode(
                    body,
                    Type::Struct(&[
                        ("id", Type::Str),
                        ("options", Type::Struct(&[("rawValue", Type::Int)])),
                    ]),
                );
                let vm_id = request
                    .as_ref()
                    .ok()
                    .and_then(|v| v.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let options = request
                    .as_ref()
                    .ok()
                    .and_then(|v| v.get("options"))
                    .and_then(|v| v.get("rawValue"))
                    .and_then(Value::as_i64);
                eprintln!("mock: start vm {vm_id} options={options:?}");
                let reply = Value::dict([(
                    "serverInfo",
                    Value::dict([
                        ("spicePortInternal", Value::Int(5900)),
                        ("spicePortExternal", Value::Int(5900)),
                        ("spiceHostExternal", Value::Str("127.0.0.1".into())),
                        ("spicePublicKey", Value::Bytes(vec![0u8; 32])),
                        ("spicePassword", Value::Str("mock-ticket".into())),
                    ]),
                )]);
                let _ = send_reply(&mut tls, id, token, &reply).await;
                let _ = transition(&mut tls, next_server_token, &vm_id, 1).await;
                next_server_token += 1;
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                let _ = transition(&mut tls, next_server_token, &vm_id, 2).await;
                next_server_token += 1;
            }
            11 | 12 | 13 | 14 => {
                eprintln!("mock: vm action id={id}");
                let _ = send_reply(&mut tls, id, token, &Value::empty()).await;
                let state = match id {
                    13 => 4, // pause -> paused
                    14 => 2, // resume -> started
                    11 => 0, // stop -> stopped
                    _ => 1,  // restart -> starting
                };
                let vm_id = "A1B2C3D4-0000-0000-0000-000000000001";
                let _ = transition(&mut tls, next_server_token, vm_id, state).await;
                next_server_token += 1;
            }
            other => {
                eprintln!("mock: unhandled message {other}");
                let _ = send_reply(&mut tls, id, token, &Value::empty()).await;
            }
        }
    }
    eprintln!("mock: client disconnected");
}

#[tokio::main]
async fn main() {
    let (cert, key) = self_signed();
    let config = rustls::ServerConfig::builder()
        .with_client_cert_verifier(Arc::new(AcceptAnyClientCert))
        .with_single_cert(vec![cert.clone()], key)
        .expect("server config");
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    println!("{}", addr.port());
    eprintln!("mock UTM server listening on {addr}");
    loop {
        let Ok((tcp, _)) = listener.accept().await else {
            break;
        };
        let acceptor = acceptor.clone();
        tokio::spawn(async move {
            if let Ok(tls) = acceptor.accept(tcp).await {
                handle_connection(tls).await;
            }
        });
    }
}
