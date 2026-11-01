//! SPICE main channel: session id discovery, channel attachment and mouse mode.

use std::sync::Arc;

use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::error::{Error, Result};
use crate::spice::link::{LinkOptions, SpiceReader, SpiceWriter};
use crate::spice::protocol::*;

#[derive(Debug, Clone, Default)]
pub struct MainInfo {
    pub session_id: u32,
    pub supported_mouse_modes: u16,
    pub current_mouse_mode: u16,
    pub agent_connected: bool,
}

pub struct MainChannel {
    pub info: MainInfo,
    pub writer: Arc<SpiceWriter>,
    pub mouse_mode: watch::Receiver<MainInfo>,
    pub task: JoinHandle<()>,
}

/// Connect the main channel and wait for `SPICE_MSG_MAIN_INIT`, then attach
/// the display/inputs channels.
pub async fn connect_main(options: LinkOptions) -> Result<MainChannel> {
    let mut channel = crate::spice::link::connect_channel(options).await?;
    let mut ack = AckState::default();
    let info = loop {
        let message = channel.reader.recv().await?;
        if ack.handle(&message, &channel.writer).await? {
            continue;
        }
        match message.ty {
            MSG_MAIN_INIT => {
                let info = parse_main_init(&message.body)?;
                debug!(
                    "SPICE session {} mouse modes {:x}/{:x}",
                    info.session_id, info.supported_mouse_modes, info.current_mouse_mode
                );
                // Ask for absolute (client) mouse mode when available.
                if info.supported_mouse_modes & MOUSE_MODE_CLIENT != 0
                    && info.current_mouse_mode != MOUSE_MODE_CLIENT
                {
                    channel
                        .writer
                        .send(
                            MSGC_MAIN_MOUSE_MODE_REQUEST,
                            &MOUSE_MODE_CLIENT.to_le_bytes(),
                        )
                        .await?;
                }
                channel.writer.send(MSGC_MAIN_ATTACH_CHANNELS, &[]).await?;
                // The server registers the client for additional channels and
                // announces the available channels in response to the attach
                // request.  Wait for that list before the caller opens the
                // display/inputs channels.
                let wait = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    loop {
                        let message = channel.reader.recv().await?;
                        if ack.handle(&message, &channel.writer).await? {
                            continue;
                        }
                        if message.ty == MSG_MAIN_CHANNELS_LIST {
                            debug!(
                                "SPICE channels list received ({} bytes)",
                                message.body.len()
                            );
                            return Ok(());
                        }
                    }
                })
                .await;
                match wait {
                    Ok(result) => result?,
                    Err(_) => debug!("timed out waiting for SPICE channels list"),
                }
                break info;
            }
            MSG_MAIN_NAME | MSG_MAIN_UUID | MSG_MAIN_CHANNELS_LIST => {}
            other => debug!("main: ignoring message {other} before init"),
        }
    };

    let (tx, rx) = watch::channel(info.clone());
    let writer = Arc::clone(&channel.writer);
    let task = tokio::spawn(main_loop(
        channel.reader,
        Arc::clone(&writer),
        tx,
        info.clone(),
    ));

    Ok(MainChannel {
        info,
        writer,
        mouse_mode: rx,
        task,
    })
}

async fn main_loop(
    mut reader: SpiceReader,
    writer: Arc<SpiceWriter>,
    tx: watch::Sender<MainInfo>,
    mut info: MainInfo,
) {
    let mut ack = AckState::default();
    loop {
        let message = match reader.recv().await {
            Ok(message) => message,
            Err(error) => {
                debug!("main channel closed: {error}");
                return;
            }
        };
        if let Err(error) = ack.handle(&message, &writer).await {
            debug!("main channel ack failed: {error}");
            return;
        }
        match message.ty {
            MSG_SET_ACK | MSG_PING | MSG_NOTIFY | MSG_WAIT_FOR_CHANNELS => {}
            MSG_DISCONNECTING => {
                warn!("SPICE server is disconnecting the main channel");
                return;
            }
            MSG_MAIN_MOUSE_MODE => {
                if message.body.len() >= 4 {
                    info.supported_mouse_modes =
                        u16::from_le_bytes([message.body[0], message.body[1]]);
                    info.current_mouse_mode =
                        u16::from_le_bytes([message.body[2], message.body[3]]);
                    let _ = tx.send(info.clone());
                }
            }
            MSG_MAIN_AGENT_CONNECTED => {
                info.agent_connected = true;
                let _ = tx.send(info.clone());
            }
            MSG_MAIN_AGENT_DISCONNECTED => {
                info.agent_connected = false;
                let _ = tx.send(info.clone());
            }
            _ => {}
        }
    }
}

fn parse_main_init(body: &[u8]) -> Result<MainInfo> {
    if body.len() < 32 {
        return Err(Error::Spice("truncated SPICE main init".into()));
    }
    Ok(MainInfo {
        session_id: u32::from_le_bytes([body[0], body[1], body[2], body[3]]),
        supported_mouse_modes: u32::from_le_bytes([body[8], body[9], body[10], body[11]]) as u16,
        current_mouse_mode: u32::from_le_bytes([body[12], body[13], body[14], body[15]]) as u16,
        agent_connected: u32::from_le_bytes([body[16], body[17], body[18], body[19]]) != 0,
    })
}

#[derive(Debug, Default)]
pub struct AckState {
    generation: u32,
    window: u32,
    count: u32,
}

impl AckState {
    /// Handle a base-channel message.  Returns true when the message was
    /// consumed.
    pub async fn handle(
        &mut self,
        message: &crate::spice::link::SpiceMessage,
        writer: &SpiceWriter,
    ) -> Result<bool> {
        match message.ty {
            MSG_SET_ACK => {
                if message.body.len() >= 8 {
                    self.generation = u32::from_le_bytes(message.body[0..4].try_into().unwrap());
                    self.window = u32::from_le_bytes(message.body[4..8].try_into().unwrap());
                    self.count = 0;
                    writer
                        .send(MSGC_ACK_SYNC, &self.generation.to_le_bytes())
                        .await?;
                }
                Ok(true)
            }
            MSG_PING => {
                // `SpiceMsgPing` is `id: u32` + `timestamp: u64`.  The ping body
                // may carry extra data (the server's 250 KiB net test), but the
                // pong struct has no payload, and the server's demarshaller
                // rejects a pong whose size does not match exactly.
                let len = message.body.len().min(12);
                writer.send(MSGC_PONG, &message.body[..len]).await?;
                Ok(true)
            }
            MSG_DISCONNECTING => Err(Error::Closed),
            MSG_NOTIFY => Ok(true),
            _ => {
                if self.window > 0 {
                    self.count += 1;
                    if self.count >= (self.window / 2).max(1) {
                        self.count = 0;
                        writer
                            .send(MSGC_ACK, &self.generation.to_le_bytes())
                            .await?;
                    }
                }
                Ok(false)
            }
        }
    }
}

/// Convenience helper used by the display/inputs channel loops.
pub async fn connect_main_channel(options: LinkOptions) -> Result<MainChannel> {
    connect_main(options).await
}
