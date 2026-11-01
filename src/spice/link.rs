//! Minimal SPICE link protocol: TLS transport, link handshake, ticket
//! authentication and mini-header message framing.
//!
//! Only the pieces needed by the web client are implemented.  The wire format
//! follows `spice-protocol/protocol.h` and spice-gtk's `spice-channel.c`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rsa::pkcs8::DecodePublicKey;
use rsa::{Oaep, RsaPublicKey};
use rustls::pki_types::ServerName;
use sha1::Sha1;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;
use tracing::{debug, trace};

use crate::error::{Error, Result};
use crate::spice::protocol::*;

const MAX_MESSAGE_SIZE: usize = 64 * 1024 * 1024;
const MAX_LINK_REPLY_SIZE: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct LinkOptions {
    pub host: String,
    pub port: u16,
    pub password: String,
    /// DER-encoded SubjectPublicKeyInfo expected from the SPICE TLS server.
    pub expected_spki: Vec<u8>,
    pub connection_id: u32,
    pub channel_type: u8,
    pub channel_id: u8,
    pub common_caps: Vec<u32>,
    pub channel_caps: Vec<u32>,
    pub timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct LinkInfo {
    pub mini_header: bool,
    pub peer_common_caps: Vec<u32>,
    pub peer_channel_caps: Vec<u32>,
}

#[derive(Debug)]
pub struct SpiceMessage {
    pub ty: u16,
    pub body: Vec<u8>,
}

/// Reading half of an established SPICE channel.
pub struct SpiceReader {
    inner: ReadHalf<TlsStream<TcpStream>>,
    mini: bool,
}

impl SpiceReader {
    pub async fn recv(&mut self) -> Result<SpiceMessage> {
        if self.mini {
            let mut header = [0u8; 6];
            self.inner.read_exact(&mut header).await?;
            let ty = u16::from_le_bytes([header[0], header[1]]);
            let size = u32::from_le_bytes([header[2], header[3], header[4], header[5]]) as usize;
            if size > MAX_MESSAGE_SIZE {
                return Err(Error::Spice(format!("message too large: {size}")));
            }
            let mut body = vec![0u8; size];
            self.inner.read_exact(&mut body).await?;
            trace!(
                "SPICE <- type {ty} ({} bytes) header={} body={}",
                body.len(),
                hex::encode(header),
                hex::encode(&body[..body.len().min(32)])
            );
            Ok(SpiceMessage { ty, body })
        } else {
            let mut header = [0u8; 18];
            self.inner.read_exact(&mut header).await?;
            let ty = u16::from_le_bytes([header[8], header[9]]);
            let size =
                u32::from_le_bytes([header[10], header[11], header[12], header[13]]) as usize;
            if size > MAX_MESSAGE_SIZE {
                return Err(Error::Spice(format!("message too large: {size}")));
            }
            let mut body = vec![0u8; size];
            self.inner.read_exact(&mut body).await?;
            trace!(
                "SPICE <- type {ty} ({} bytes) header={} body={}",
                body.len(),
                hex::encode(header),
                hex::encode(&body[..body.len().min(32)])
            );
            Ok(SpiceMessage { ty, body })
        }
    }
}

/// Writing half of an established SPICE channel.  Frames are queued to a
/// dedicated writer task so input handling never blocks on the socket.
pub struct SpiceWriter {
    tx: mpsc::Sender<Vec<u8>>,
    mini: bool,
    serial: AtomicU64,
}

impl SpiceWriter {
    pub async fn send(&self, ty: u16, body: &[u8]) -> Result<()> {
        let frame = if self.mini {
            let mut out = Vec::with_capacity(body.len() + 6);
            out.extend_from_slice(&ty.to_le_bytes());
            out.extend_from_slice(&(body.len() as u32).to_le_bytes());
            out.extend_from_slice(body);
            out
        } else {
            let serial = self.serial.fetch_add(1, Ordering::Relaxed);
            let mut out = Vec::with_capacity(body.len() + 18);
            out.extend_from_slice(&serial.to_le_bytes());
            out.extend_from_slice(&ty.to_le_bytes());
            out.extend_from_slice(&(body.len() as u32).to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(body);
            out
        };
        self.tx.send(frame).await.map_err(|_| Error::Closed)
    }
}

pub struct Channel {
    pub reader: SpiceReader,
    pub writer: Arc<SpiceWriter>,
    pub info: LinkInfo,
}

/// Connect a SPICE channel over TLS, perform the link handshake and ticket
/// authentication.
pub async fn connect_channel(options: LinkOptions) -> Result<Channel> {
    let tcp = tokio::time::timeout(
        options.timeout,
        crate::net::connect(&options.host, options.port),
    )
    .await
    .map_err(|_| Error::Timeout)?
    .map_err(Error::Io)?;
    tcp.set_nodelay(true).ok();

    let config = crate::cert::spice_client_config(options.expected_spki.clone())?;
    let connector = TlsConnector::from(Arc::new(config));
    let server_name = crate::net::tls_server_name(&options.host)
        .or_else(|| ServerName::try_from("utm-spice.local").ok())
        .ok_or_else(|| Error::Spice("invalid SPICE server name".into()))?;
    let mut tls = tokio::time::timeout(options.timeout, connector.connect(server_name, tcp))
        .await
        .map_err(|_| Error::Timeout)?
        .map_err(|e| Error::Spice(format!("TLS handshake failed: {e}")))?;

    let common_caps = if options.common_caps.is_empty() {
        vec![
            (1 << COMMON_CAP_PROTOCOL_AUTH_SELECTION)
                | (1 << COMMON_CAP_AUTH_SPICE)
                | (1 << COMMON_CAP_MINI_HEADER),
        ]
    } else {
        options.common_caps.clone()
    };
    let channel_caps = options.channel_caps.clone();

    let mut link_body = Vec::with_capacity(18 + 4 * (common_caps.len() + channel_caps.len()));
    link_body.extend_from_slice(&options.connection_id.to_le_bytes());
    link_body.push(options.channel_type);
    link_body.push(options.channel_id);
    link_body.extend_from_slice(&(common_caps.len() as u32).to_le_bytes());
    link_body.extend_from_slice(&(channel_caps.len() as u32).to_le_bytes());
    link_body.extend_from_slice(&18u32.to_le_bytes());
    for cap in &common_caps {
        link_body.extend_from_slice(&cap.to_le_bytes());
    }
    for cap in &channel_caps {
        link_body.extend_from_slice(&cap.to_le_bytes());
    }

    trace!(
        "SPICE link body channel {} connection_id {} body={}",
        options.channel_type,
        options.connection_id,
        hex::encode(&link_body)
    );
    let mut header = Vec::with_capacity(16);
    header.extend_from_slice(SPICE_MAGIC);
    header.extend_from_slice(&SPICE_VERSION_MAJOR.to_le_bytes());
    header.extend_from_slice(&SPICE_VERSION_MINOR.to_le_bytes());
    header.extend_from_slice(&(link_body.len() as u32).to_le_bytes());
    tls.write_all(&header).await?;
    tls.write_all(&link_body).await?;
    tls.flush().await?;
    debug!(
        "SPICE channel {}:{} type {} id {} link sent",
        options.host, options.port, options.channel_type, options.channel_id
    );

    let mut reply_header = [0u8; 16];
    tls.read_exact(&mut reply_header).await?;
    if &reply_header[0..4] != SPICE_MAGIC {
        return Err(Error::Spice("invalid SPICE reply magic".into()));
    }
    let major = u32::from_le_bytes([
        reply_header[4],
        reply_header[5],
        reply_header[6],
        reply_header[7],
    ]);
    if major != SPICE_VERSION_MAJOR {
        return Err(Error::Spice(format!(
            "SPICE major version {major} is not supported"
        )));
    }
    let reply_size = u32::from_le_bytes([
        reply_header[12],
        reply_header[13],
        reply_header[14],
        reply_header[15],
    ]) as usize;
    if !(LINK_REPLY_FIXED_SIZE..=MAX_LINK_REPLY_SIZE).contains(&reply_size) {
        return Err(Error::Spice(format!(
            "invalid SPICE link reply size {reply_size}"
        )));
    }
    let mut reply = vec![0u8; reply_size];
    tls.read_exact(&mut reply).await?;

    trace!(
        "SPICE link reply channel {} header={} first32={}",
        options.channel_type,
        hex::encode(reply_header),
        hex::encode(&reply[..reply.len().min(32)])
    );
    let link_error = u32::from_le_bytes([reply[0], reply[1], reply[2], reply[3]]);
    if link_error != 0 {
        return Err(Error::Spice(format!(
            "SPICE link error {link_error}{}",
            if link_error == 1 {
                " (server requires TLS)"
            } else {
                ""
            }
        )));
    }
    let mut pub_key = [0u8; TICKET_PUBKEY_BYTES];
    pub_key.copy_from_slice(&reply[4..4 + TICKET_PUBKEY_BYTES]);
    let num_common = u32::from_le_bytes([reply[166], reply[167], reply[168], reply[169]]) as usize;
    let num_channel = u32::from_le_bytes([reply[170], reply[171], reply[172], reply[173]]) as usize;
    let caps_offset = u32::from_le_bytes([reply[174], reply[175], reply[176], reply[177]]) as usize;
    let mut peer_common_caps = Vec::with_capacity(num_common);
    let mut peer_channel_caps = Vec::with_capacity(num_channel);
    let caps_end = reply.len();
    for i in 0..num_common {
        let start = caps_offset + i * 4;
        if start + 4 > caps_end {
            return Err(Error::Spice("truncated SPICE common caps".into()));
        }
        peer_common_caps.push(u32::from_le_bytes([
            reply[start],
            reply[start + 1],
            reply[start + 2],
            reply[start + 3],
        ]));
    }
    for i in 0..num_channel {
        let start = caps_offset + (num_common + i) * 4;
        if start + 4 > caps_end {
            return Err(Error::Spice("truncated SPICE channel caps".into()));
        }
        peer_channel_caps.push(u32::from_le_bytes([
            reply[start],
            reply[start + 1],
            reply[start + 2],
            reply[start + 3],
        ]));
    }

    if has_cap(&peer_common_caps, COMMON_CAP_PROTOCOL_AUTH_SELECTION) {
        if !has_cap(&peer_common_caps, COMMON_CAP_AUTH_SPICE) {
            return Err(Error::Spice(
                "SPICE server does not offer ticket authentication".into(),
            ));
        }
        tls.write_all(&AUTH_MECHANISM_SPICE.to_le_bytes()).await?;
    }

    // Ticket: RSA-OAEP(SHA-1) over password + NUL.
    let public_key = RsaPublicKey::from_public_key_der(&pub_key)
        .map_err(|e| Error::Spice(format!("invalid SPICE ticket public key: {e}")))?;
    let mut password = options.password.as_bytes().to_vec();
    if password.len() > 60 {
        return Err(Error::Spice("SPICE password is too long".into()));
    }
    password.push(0);
    let mut rng = rand::rngs::OsRng;
    let encrypted = public_key
        .encrypt(&mut rng, Oaep::new::<Sha1>(), &password)
        .map_err(|e| Error::Spice(format!("SPICE ticket encryption failed: {e}")))?;
    if encrypted.len() != TICKET_SIZE {
        return Err(Error::Spice(format!(
            "unexpected SPICE ticket size {}",
            encrypted.len()
        )));
    }
    tls.write_all(&encrypted).await?;
    tls.flush().await?;

    let mut auth_result = [0u8; 4];
    tls.read_exact(&mut auth_result).await?;
    let auth_result = u32::from_le_bytes(auth_result);
    trace!(
        "SPICE auth result channel {}: {}",
        options.channel_type,
        auth_result
    );
    if auth_result != 0 {
        return Err(Error::Auth(format!(
            "SPICE ticket authentication failed (result {auth_result})"
        )));
    }

    let mini_header = has_cap(&peer_common_caps, COMMON_CAP_MINI_HEADER);
    debug!(
        "SPICE channel type {} ready (mini header: {})",
        options.channel_type, mini_header
    );

    let (read, mut write) = tokio::io::split(tls);
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(512);
    tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if let Err(error) = write.write_all(&frame).await {
                debug!("SPICE writer stopped: {error}");
                break;
            }
            if let Err(error) = write.flush().await {
                debug!("SPICE writer flush failed: {error}");
                break;
            }
        }
    });

    Ok(Channel {
        reader: SpiceReader {
            inner: read,
            mini: mini_header,
        },
        writer: Arc::new(SpiceWriter {
            tx,
            mini: mini_header,
            serial: AtomicU64::new(1),
        }),
        info: LinkInfo {
            mini_header,
            peer_common_caps,
            peer_channel_caps,
        },
    })
}
