use serde::Serialize;

use crate::codable::Value;

pub const SERVER_PORT_DEFAULT: u16 = 21589;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum VmState {
    Stopped = 0,
    Starting = 1,
    Started = 2,
    Pausing = 3,
    Paused = 4,
    Resuming = 5,
    Saving = 6,
    Restoring = 7,
    Stopping = 8,
}

impl VmState {
    pub fn as_str(self) -> &'static str {
        match self {
            VmState::Stopped => "stopped",
            VmState::Starting => "starting",
            VmState::Started => "started",
            VmState::Pausing => "pausing",
            VmState::Paused => "paused",
            VmState::Resuming => "resuming",
            VmState::Saving => "saving",
            VmState::Restoring => "restoring",
            VmState::Stopping => "stopping",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "stopped" => VmState::Stopped,
            "starting" => VmState::Starting,
            "started" => VmState::Started,
            "pausing" => VmState::Pausing,
            "paused" => VmState::Paused,
            "resuming" => VmState::Resuming,
            "saving" => VmState::Saving,
            "restoring" => VmState::Restoring,
            "stopping" => VmState::Stopping,
            _ => return None,
        })
    }

    pub fn from_i64(value: i64) -> Self {
        match value {
            0 => VmState::Stopped,
            1 => VmState::Starting,
            2 => VmState::Started,
            3 => VmState::Pausing,
            4 => VmState::Paused,
            5 => VmState::Resuming,
            6 => VmState::Saving,
            7 => VmState::Restoring,
            8 => VmState::Stopping,
            _ => VmState::Stopped,
        }
    }
}

impl Serialize for VmState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopMethod {
    Request = 0,
    Force = 1,
    Kill = 2,
}

impl StopMethod {
    pub fn from_i64(value: i64) -> Self {
        match value {
            1 => StopMethod::Force,
            2 => StopMethod::Kill,
            _ => StopMethod::Request,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartOptions(pub i64);

impl StartOptions {
    pub const NONE: StartOptions = StartOptions(0);
    pub const BOOT_DISPOSABLE: StartOptions = StartOptions(1);
    pub const BOOT_RECOVERY: StartOptions = StartOptions(2);
    pub const REMOTE_SESSION: StartOptions = StartOptions(4);
}

#[derive(Debug, Clone, Serialize)]
pub struct VmInfo {
    pub id: String,
    pub name: String,
    pub path: String,
    pub is_shortcut: bool,
    pub is_suspended: bool,
    pub is_takeover_allowed: bool,
    pub backend: String,
    pub state: VmState,
    pub mounted_drives: std::collections::BTreeMap<String, String>,
}

impl VmInfo {
    pub fn from_value(value: &Value) -> Option<Self> {
        let id = value.get("id")?.as_str()?.to_owned();
        Some(Self {
            id,
            name: value
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            path: value
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            is_shortcut: value
                .get("isShortcut")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            is_suspended: value
                .get("isSuspended")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            is_takeover_allowed: value
                .get("isTakeoverAllowed")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            backend: value
                .get("backend")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_lowercase(),
            state: value
                .get("state")
                .and_then(Value::as_str)
                .and_then(VmState::from_name)
                .unwrap_or(VmState::Stopped),
            mounted_drives: value
                .get("mountedDrives")
                .and_then(|v| match v {
                    Value::Dict(map) => Some(
                        map.iter()
                            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                            .collect(),
                    ),
                    _ => None,
                })
                .unwrap_or_default(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct ServerInfo {
    pub spice_port_internal: u16,
    pub spice_port_external: u16,
    pub spice_host_external: String,
    pub spice_public_key: Vec<u8>,
    pub spice_password: String,
}

impl ServerInfo {
    pub fn from_value(value: &Value) -> Option<Self> {
        Some(Self {
            spice_port_internal: value
                .get("spicePortInternal")
                .and_then(Value::as_i64)
                .unwrap_or(0) as u16,
            spice_port_external: value
                .get("spicePortExternal")
                .and_then(Value::as_i64)
                .unwrap_or(0) as u16,
            spice_host_external: value
                .get("spiceHostExternal")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            spice_public_key: value
                .get("spicePublicKey")
                .and_then(Value::as_bytes)
                .unwrap_or_default()
                .to_vec(),
            spice_password: value
                .get("spicePassword")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        })
    }

    /// Returns the host/port a browser-side client would use, matching the
    /// pyutmremote GUI logic: prefer the externally advertised endpoint, fall
    /// back to the UTM server address and internal SPICE port.
    pub fn endpoint(&self, server_host: &str) -> Option<(String, u16)> {
        if self.spice_port_external != 0 {
            let host = if self.spice_host_external.is_empty() {
                server_host
            } else {
                &self.spice_host_external
            };
            Some((host.to_owned(), self.spice_port_external))
        } else if self.spice_port_internal != 0 {
            Some((server_host.to_owned(), self.spice_port_internal))
        } else {
            None
        }
    }
}
