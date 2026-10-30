//! SPICE inputs channel messages.

use crate::error::Result;
use crate::spice::link::SpiceWriter;
use crate::spice::protocol::*;

#[derive(Debug, Clone)]
pub enum InputCommand {
    Key {
        code: String,
        down: bool,
    },
    KeyModifiers {
        modifiers: u16,
    },
    MousePosition {
        x: u32,
        y: u32,
        buttons: u16,
        display_id: u8,
    },
    MouseMotion {
        dx: i32,
        dy: i32,
        buttons: u16,
    },
    MousePress {
        button: u8,
        buttons: u16,
    },
    MouseRelease {
        button: u8,
        buttons: u16,
    },
}

impl InputCommand {
    pub async fn send(&self, writer: &SpiceWriter) -> Result<()> {
        match self {
            InputCommand::Key { code, down } => {
                let Some(scancode) = crate::spice::keymap::scancode_for_code(code) else {
                    return Ok(());
                };
                let scancode = make_scancode(scancode, !*down);
                let ty = if *down {
                    MSGC_INPUTS_KEY_DOWN
                } else {
                    MSGC_INPUTS_KEY_UP
                };
                writer.send(ty, &scancode.to_le_bytes()).await
            }
            InputCommand::KeyModifiers { modifiers } => {
                writer
                    .send(MSGC_INPUTS_KEY_MODIFIERS, &modifiers.to_le_bytes())
                    .await
            }
            InputCommand::MousePosition {
                x,
                y,
                buttons,
                display_id,
            } => {
                let mut body = Vec::with_capacity(11);
                body.extend_from_slice(&x.to_le_bytes());
                body.extend_from_slice(&y.to_le_bytes());
                body.extend_from_slice(&buttons.to_le_bytes());
                body.push(*display_id);
                writer.send(MSGC_INPUTS_MOUSE_POSITION, &body).await
            }
            InputCommand::MouseMotion { dx, dy, buttons } => {
                let mut body = Vec::with_capacity(10);
                body.extend_from_slice(&dx.to_le_bytes());
                body.extend_from_slice(&dy.to_le_bytes());
                body.extend_from_slice(&buttons.to_le_bytes());
                writer.send(MSGC_INPUTS_MOUSE_MOTION, &body).await
            }
            InputCommand::MousePress { button, buttons } => {
                let mut body = Vec::with_capacity(3);
                body.push(*button);
                body.extend_from_slice(&buttons.to_le_bytes());
                writer.send(MSGC_INPUTS_MOUSE_PRESS, &body).await
            }
            InputCommand::MouseRelease { button, buttons } => {
                let mut body = Vec::with_capacity(3);
                body.push(*button);
                body.extend_from_slice(&buttons.to_le_bytes());
                writer.send(MSGC_INPUTS_MOUSE_RELEASE, &body).await
            }
        }
    }
}
