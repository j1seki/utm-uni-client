//! A small, native SPICE client used to render UTM Remote VM displays.

pub mod display;
pub mod image;
pub mod inputs;
pub mod keymap;
pub mod link;
pub mod main;
pub mod protocol;
pub mod surface;

use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::error::{Error, Result};
use crate::spice::display::{DisplayRenderer, DisplayUpdate};
use crate::spice::link::{SpiceReader, SpiceWriter};
use crate::spice::main::AckState;

pub use link::{connect_channel, Channel, LinkInfo, LinkOptions, SpiceMessage};
pub use main::{connect_main, MainChannel, MainInfo};

/// Read a display channel until it closes, rendering every message and
/// forwarding updates to the session's frame sink.
pub async fn run_display_channel(
    mut reader: SpiceReader,
    writer: Arc<SpiceWriter>,
    mut renderer: DisplayRenderer,
    updates: mpsc::Sender<DisplayUpdate>,
) {
    let mut ack = AckState::default();
    loop {
        let message = match reader.recv().await {
            Ok(message) => message,
            Err(error) => {
                debug!("display channel closed: {error}");
                let _ = updates
                    .send(DisplayUpdate::Unsupported(error.to_string()))
                    .await;
                return;
            }
        };
        match ack.handle(&message, &writer).await {
            Ok(true) => continue,
            Ok(false) => {}
            Err(error) => {
                debug!("display channel ack failed: {error}");
                return;
            }
        }
        match renderer.handle(message.ty, &message.body) {
            Ok(updates_list) => {
                for update in updates_list {
                    if updates.send(update).await.is_err() {
                        return;
                    }
                }
            }
            Err(error) => {
                warn!("display message {} failed: {error}", message.ty);
                if updates
                    .send(DisplayUpdate::Unsupported(error.to_string()))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
    }
}

/// Read an inputs channel, handling base messages and server-side state
/// notifications.  Outgoing input commands are sent by a separate task.
pub async fn run_inputs_channel(mut reader: SpiceReader, writer: Arc<SpiceWriter>) {
    let mut ack = AckState::default();
    loop {
        let message = match reader.recv().await {
            Ok(message) => message,
            Err(error) => {
                debug!("inputs channel closed: {error}");
                return;
            }
        };
        match ack.handle(&message, &writer).await {
            Ok(true) => continue,
            Ok(false) => {}
            Err(error) => {
                debug!("inputs channel ack failed: {error}");
                return;
            }
        }
        match message.ty {
            protocol::MSG_INPUTS_INIT | protocol::MSG_INPUTS_KEY_MODIFIERS => {
                if message.body.len() >= 2 {
                    debug!(
                        "inputs modifiers: {:04x}",
                        u16::from_le_bytes([message.body[0], message.body[1]])
                    );
                }
            }
            protocol::MSG_INPUTS_MOUSE_MOTION_ACK => {}
            _ => {}
        }
    }
}

/// Send the display channel initialization and preferred-compression messages.
pub async fn initialize_display(writer: &SpiceWriter, pref_compression: bool) -> Result<()> {
    let mut body = Vec::with_capacity(14);
    body.push(1); // pixmap cache id
    body.extend_from_slice(&(4 * 1024 * 1024i64).to_le_bytes()); // pixels
    body.push(1); // glz dictionary id
    body.extend_from_slice(&0i32.to_le_bytes()); // glz window
    writer.send(protocol::MSGC_DISPLAY_INIT, &body).await?;
    if pref_compression {
        writer
            .send(
                protocol::MSGC_DISPLAY_PREFERRED_COMPRESSION,
                &[protocol::IMAGE_COMPRESSION_OFF],
            )
            .await?;
    }
    Ok(())
}

pub fn display_channel_caps() -> Vec<u32> {
    vec![
        (1 << protocol::DISPLAY_CAP_MONITORS_CONFIG)
            | (1 << protocol::DISPLAY_CAP_PREF_COMPRESSION),
    ]
}

pub fn inputs_channel_caps() -> Vec<u32> {
    vec![]
}

pub fn main_channel_caps() -> Vec<u32> {
    vec![]
}

/// Base SPICE common caps advertised by the client.
pub fn common_caps() -> Vec<u32> {
    vec![
        (1 << protocol::COMMON_CAP_PROTOCOL_AUTH_SELECTION)
            | (1 << protocol::COMMON_CAP_AUTH_SPICE)
            | (1 << protocol::COMMON_CAP_MINI_HEADER),
    ]
}

/// Small helper for sessions that need to distinguish a closed channel from a
/// protocol failure.
pub fn is_closed(error: &Error) -> bool {
    matches!(error, Error::Closed | Error::Io(_))
}
