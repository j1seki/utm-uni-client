//! SwiftConnect-style framed peer used by UTM Remote.
//!
//! Wire framing is an 8-byte big-endian length followed by:
//! `u8 message_id`, `u8 flags`, `uleb128 token`, Codable payload.
//! Requests from the server are answered with the same token and the
//! `response` flag set.  Requests from the client carry a monotonically
//! increasing token and are matched against a pending-request map.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio_rustls::client::TlsStream;
use tracing::{debug, warn};

use crate::codable::{decode, encode, Value};
use crate::error::{Error, Result};
use crate::utm::messages::{self, client, schemas};

const FLAG_RESPONSE: u8 = 1;
const FLAG_ERROR: u8 = 2;
const MAX_FRAME_SIZE: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, thiserror::Error)]
pub enum PeerError {
    #[error("connection closed")]
    Closed,
    #[error("remote error: {0}")]
    Remote(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("request timed out")]
    Timeout,
}

impl From<PeerError> for Error {
    fn from(value: PeerError) -> Self {
        match value {
            PeerError::Closed => Error::Closed,
            PeerError::Remote(message) => Error::Remote(message),
            PeerError::Protocol(message) => Error::Protocol(message),
            PeerError::Timeout => Error::Timeout,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ServerEvent {
    ListChanged(Vec<String>),
    QemuConfigurationChanged(String),
    MountedDrivesChanged(String, std::collections::BTreeMap<String, String>),
    VmTransition {
        id: String,
        state: String,
        is_takeover_allowed: bool,
    },
    VmError {
        id: String,
        message: String,
    },
    Disconnected(String),
}

enum WriteCmd {
    Frame(Vec<u8>),
    Close,
}

type PendingRequest = oneshot::Sender<std::result::Result<Vec<u8>, PeerError>>;

pub struct Peer {
    tx: mpsc::Sender<WriteCmd>,
    pending: Mutex<HashMap<u64, PendingRequest>>,
    next_token: AtomicU64,
    trusted: AtomicBool,
    closed: AtomicBool,
    events: mpsc::UnboundedSender<ServerEvent>,
    request_timeout: Duration,
}

impl Peer {
    /// Wrap an established TLS stream and spawn the reader/writer tasks.
    pub fn spawn(
        stream: TlsStream<TcpStream>,
        events: mpsc::UnboundedSender<ServerEvent>,
    ) -> Arc<Self> {
        let (read, write) = tokio::io::split(stream);
        let (tx, rx) = mpsc::channel::<WriteCmd>(64);
        let peer = Arc::new(Self {
            tx,
            pending: Mutex::new(HashMap::new()),
            next_token: AtomicU64::new(1),
            trusted: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            events,
            request_timeout: Duration::from_secs(45),
        });

        let reader_peer = Arc::clone(&peer);
        tokio::spawn(async move {
            reader_loop(read, reader_peer).await;
        });
        let writer_peer = Arc::downgrade(&peer);
        tokio::spawn(async move {
            writer_loop(write, rx, writer_peer).await;
        });

        peer
    }

    pub fn set_trusted(&self) {
        self.trusted.store(true, Ordering::SeqCst);
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub fn close(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            let _ = self.tx.try_send(WriteCmd::Close);
        }
        self.fail_all(PeerError::Closed);
    }

    pub async fn request(&self, id: u8, body: &Value) -> Result<Vec<u8>> {
        let encoded = encode(body)?;
        self.request_raw(id, encoded).await
    }

    pub async fn request_raw(&self, id: u8, body: Vec<u8>) -> Result<Vec<u8>> {
        if self.is_closed() {
            return Err(Error::Closed);
        }
        let token = self.next_token.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().expect("pending mutex poisoned");
            pending.insert(token, tx);
        }

        let mut payload = Vec::with_capacity(body.len() + 16);
        payload.push(id);
        payload.push(0);
        push_uleb(&mut payload, token);
        payload.extend_from_slice(&body);

        if self.tx.send(WriteCmd::Frame(payload)).await.is_err() {
            self.pending
                .lock()
                .expect("pending mutex poisoned")
                .remove(&token);
            return Err(Error::Closed);
        }

        match tokio::time::timeout(self.request_timeout, rx).await {
            Ok(Ok(Ok(body))) => Ok(body),
            Ok(Ok(Err(err))) => Err(err.into()),
            Ok(Err(_)) => Err(Error::Closed),
            Err(_) => {
                self.pending
                    .lock()
                    .expect("pending mutex poisoned")
                    .remove(&token);
                Err(Error::Timeout)
            }
        }
    }

    pub async fn reply(&self, id: u8, token: u64, body: &Value) -> Result<()> {
        let encoded = encode(body)?;
        self.send(id, FLAG_RESPONSE, token, encoded).await
    }

    pub async fn reply_error(&self, id: u8, token: u64, message: &str) {
        let _ = self
            .send(
                id,
                FLAG_RESPONSE | FLAG_ERROR,
                token,
                message.as_bytes().to_vec(),
            )
            .await;
    }

    async fn send(&self, id: u8, flags: u8, token: u64, body: Vec<u8>) -> Result<()> {
        let mut payload = Vec::with_capacity(body.len() + 16);
        payload.push(id);
        payload.push(flags);
        push_uleb(&mut payload, token);
        payload.extend_from_slice(&body);
        self.tx
            .send(WriteCmd::Frame(payload))
            .await
            .map_err(|_| Error::Closed)
    }

    fn fail_all(&self, error: PeerError) {
        let mut pending = self.pending.lock().expect("pending mutex poisoned");
        for (_, tx) in pending.drain() {
            let _ = tx.send(Err(error.clone()));
        }
    }

    async fn handle_frame(self: &Arc<Self>, payload: Vec<u8>) {
        if payload.len() < 3 {
            warn!("short UTM frame ({} bytes)", payload.len());
            return;
        }
        let id = payload[0];
        let flags = payload[1];
        let mut pos = 2;
        let token = match read_uleb(&payload, &mut pos) {
            Some(token) => token,
            None => {
                warn!("invalid ULEB128 token");
                return;
            }
        };
        let body = &payload[pos..];

        if flags & FLAG_RESPONSE != 0 {
            let pending = self
                .pending
                .lock()
                .expect("pending mutex poisoned")
                .remove(&token);
            if let Some(tx) = pending {
                if flags & FLAG_ERROR != 0 {
                    let message = String::from_utf8_lossy(body).into_owned();
                    let _ = tx.send(Err(PeerError::Remote(message)));
                } else {
                    let _ = tx.send(Ok(body.to_vec()));
                }
            } else {
                debug!("reply for unknown token {token}");
            }
            return;
        }

        if id != 0 && !self.trusted.load(Ordering::SeqCst) {
            warn!("rejecting message {id} on untrusted connection");
            return;
        }

        let peer = Arc::clone(self);
        let body = body.to_vec();
        tokio::spawn(async move {
            if let Err(error) = peer.handle_request(id, token, body).await {
                warn!("failed to handle UTM message {id}: {error}");
                peer.reply_error(id, token, &error.to_string()).await;
            }
        });
    }

    async fn handle_request(self: &Arc<Self>, id: u8, token: u64, body: Vec<u8>) -> Result<()> {
        match id {
            client::HANDSHAKE => {
                let value = decode(&body, schemas::CLIENT_HANDSHAKE_REQUEST)?;
                let version = value.get("version").and_then(Value::as_i64).unwrap_or(1);
                debug!("server client-handshake (version {version})");
                self.reply(id, token, &messages::client_handshake_reply(1, 0))
                    .await?;
            }
            client::LIST_HAS_CHANGED => {
                let value = decode(&body, schemas::LIST_HAS_CHANGED_REQUEST)?;
                let ids = string_array(value.get("ids"));
                let _ = self.events.send(ServerEvent::ListChanged(ids));
                self.reply(id, token, &messages::empty()).await?;
            }
            client::QEMU_CONFIGURATION_HAS_CHANGED => {
                let value = decode(&body, schemas::QEMU_CONFIGURATION_HAS_CHANGED_REQUEST)?;
                if let Some(vm) = value.get("id").and_then(Value::as_str) {
                    let _ = self
                        .events
                        .send(ServerEvent::QemuConfigurationChanged(vm.to_owned()));
                }
                self.reply(id, token, &messages::empty()).await?;
            }
            client::MOUNTED_DRIVES_HAS_CHANGED => {
                let value = decode(&body, schemas::MOUNTED_DRIVES_HAS_CHANGED_REQUEST)?;
                if let Some(vm) = value.get("id").and_then(Value::as_str) {
                    let mut drives = std::collections::BTreeMap::new();
                    if let Some(Value::Dict(map)) = value.get("mountedDrives") {
                        for (k, v) in map {
                            if let Some(v) = v.as_str() {
                                drives.insert(k.clone(), v.to_owned());
                            }
                        }
                    }
                    let _ = self
                        .events
                        .send(ServerEvent::MountedDrivesChanged(vm.to_owned(), drives));
                }
                self.reply(id, token, &messages::empty()).await?;
            }
            client::VIRTUAL_MACHINE_DID_TRANSITION => {
                let value = decode(&body, schemas::VM_DID_TRANSITION_REQUEST)?;
                if let Some(vm) = value.get("id").and_then(Value::as_str) {
                    let state = value
                        .get("state")
                        .and_then(Value::as_str)
                        .unwrap_or("stopped")
                        .to_owned();
                    let allowed = value
                        .get("isTakeoverAllowed")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let _ = self.events.send(ServerEvent::VmTransition {
                        id: vm.to_owned(),
                        state,
                        is_takeover_allowed: allowed,
                    });
                }
                self.reply(id, token, &messages::empty()).await?;
            }
            client::VIRTUAL_MACHINE_DID_ERROR => {
                let value = decode(&body, schemas::VM_DID_ERROR_REQUEST)?;
                if let Some(vm) = value.get("id").and_then(Value::as_str) {
                    let message = value
                        .get("errorMessage")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error")
                        .to_owned();
                    let _ = self.events.send(ServerEvent::VmError {
                        id: vm.to_owned(),
                        message,
                    });
                }
                self.reply(id, token, &messages::empty()).await?;
            }
            other => {
                return Err(Error::Unsupported(format!(
                    "unsupported client message id {other}"
                )));
            }
        }
        Ok(())
    }
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

async fn reader_loop(mut read: ReadHalf<TlsStream<TcpStream>>, peer: Arc<Peer>) {
    let mut header = [0u8; 8];
    loop {
        if let Err(error) = read.read_exact(&mut header).await {
            debug!("UTM reader stopped: {error}");
            break;
        }
        let len = u64::from_be_bytes(header) as usize;
        if len > MAX_FRAME_SIZE {
            warn!("UTM frame too large ({len} bytes)");
            break;
        }
        let mut payload = vec![0u8; len];
        if let Err(error) = read.read_exact(&mut payload).await {
            debug!("UTM reader stopped while reading body: {error}");
            break;
        }
        peer.handle_frame(payload).await;
    }
    peer.closed.store(true, Ordering::SeqCst);
    peer.fail_all(PeerError::Closed);
    let _ = peer
        .events
        .send(ServerEvent::Disconnected("connection closed".into()));
}

async fn writer_loop(
    mut write: WriteHalf<TlsStream<TcpStream>>,
    mut rx: mpsc::Receiver<WriteCmd>,
    peer: std::sync::Weak<Peer>,
) {
    while let Some(cmd) = rx.recv().await {
        match cmd {
            WriteCmd::Frame(payload) => {
                let len = (payload.len() as u64).to_be_bytes();
                if let Err(error) = write.write_all(&len).await {
                    debug!("UTM writer stopped: {error}");
                    break;
                }
                if let Err(error) = write.write_all(&payload).await {
                    debug!("UTM writer stopped: {error}");
                    break;
                }
                if let Err(error) = write.flush().await {
                    debug!("UTM writer flush failed: {error}");
                    break;
                }
            }
            WriteCmd::Close => {
                let _ = write.shutdown().await;
                break;
            }
        }
    }
    if let Some(peer) = peer.upgrade() {
        peer.closed.store(true, Ordering::SeqCst);
        peer.fail_all(PeerError::Closed);
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
