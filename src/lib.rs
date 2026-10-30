//! utm-web: a small self-hosted web client for Apple UTM Remote.
//!
//! The crate is split into the UTM Remote protocol implementation (`utm`), a
//! minimal native SPICE client (`spice`), and the HTTP/WebSocket server.

pub mod agent;
pub mod cert;
pub mod codable;
pub mod config;
pub mod display_session;
pub mod error;
pub mod profiles;
pub mod server;
pub mod spice;
pub mod utm;

pub use error::{Error, Result};
