//! UTM Remote message definitions, ported from pyutmremote's
//! `utmremotemessage.py` and `utmconfiguration.py`.

use std::collections::BTreeMap;

use crate::codable::{Type, Value};

// ---------------------------------------------------------------------------
// Server message IDs (requests sent by the client)
// ---------------------------------------------------------------------------

pub mod server {
    pub const HANDSHAKE: u8 = 0;
    pub const LIST_VIRTUAL_MACHINES: u8 = 1;
    pub const REORDER_VIRTUAL_MACHINES: u8 = 2;
    pub const GET_VIRTUAL_MACHINE_INFORMATION: u8 = 3;
    pub const GET_QEMU_CONFIGURATION: u8 = 4;
    pub const GET_PACKAGE_SIZE: u8 = 5;
    pub const GET_PACKAGE_FILE: u8 = 6;
    pub const SEND_PACKAGE_FILE: u8 = 7;
    pub const DELETE_PACKAGE_FILE: u8 = 8;
    pub const MOUNT_GUEST_TOOLS: u8 = 9;
    pub const START_VIRTUAL_MACHINE: u8 = 10;
    pub const STOP_VIRTUAL_MACHINE: u8 = 11;
    pub const RESTART_VIRTUAL_MACHINE: u8 = 12;
    pub const PAUSE_VIRTUAL_MACHINE: u8 = 13;
    pub const RESUME_VIRTUAL_MACHINE: u8 = 14;
    pub const SAVE_SNAPSHOT: u8 = 15;
    pub const DELETE_SNAPSHOT: u8 = 16;
    pub const RESTORE_SNAPSHOT: u8 = 17;
    pub const CHANGE_POINTER_TYPE: u8 = 18;
}

// ---------------------------------------------------------------------------
// Client message IDs (requests sent by the server)
// ---------------------------------------------------------------------------

pub mod client {
    pub const HANDSHAKE: u8 = 0;
    pub const LIST_HAS_CHANGED: u8 = 1;
    pub const QEMU_CONFIGURATION_HAS_CHANGED: u8 = 2;
    pub const MOUNTED_DRIVES_HAS_CHANGED: u8 = 3;
    pub const VIRTUAL_MACHINE_DID_TRANSITION: u8 = 4;
    pub const VIRTUAL_MACHINE_DID_ERROR: u8 = 5;
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

pub fn empty() -> Value {
    Value::empty()
}

pub fn handshake(version: i64, password: Option<&str>) -> Value {
    let mut map = BTreeMap::new();
    map.insert("version".to_owned(), Value::Int(version));
    if let Some(password) = password {
        map.insert("password".to_owned(), Value::Str(password.to_owned()));
    }
    Value::Dict(map)
}

pub fn client_handshake_reply(version: i64, capabilities: i64) -> Value {
    Value::dict([
        ("version", Value::Int(version)),
        ("capabilities", Value::Int(capabilities)),
    ])
}

pub fn list_virtual_machines() -> Value {
    Value::empty()
}

pub fn reorder_virtual_machines(ids: &[String], offset: i64) -> Value {
    Value::dict([
        (
            "ids",
            Value::Array(ids.iter().cloned().map(Value::Str).collect()),
        ),
        ("offset", Value::Int(offset)),
    ])
}

pub fn get_virtual_machine_information(ids: &[String]) -> Value {
    Value::dict([(
        "ids",
        Value::Array(ids.iter().cloned().map(Value::Str).collect()),
    )])
}

pub fn get_qemu_configuration(id: &str) -> Value {
    Value::dict([("id", Value::Str(id.to_owned()))])
}

pub fn get_package_size(id: &str) -> Value {
    Value::dict([("id", Value::Str(id.to_owned()))])
}

pub fn get_package_file(id: &str, path: &[String], last_modified: Option<&str>) -> Value {
    let mut map = BTreeMap::new();
    map.insert("id".to_owned(), Value::Str(id.to_owned()));
    map.insert(
        "relativePathComponents".to_owned(),
        Value::Array(path.iter().cloned().map(Value::Str).collect()),
    );
    if let Some(last_modified) = last_modified {
        map.insert(
            "lastModified".to_owned(),
            Value::Str(last_modified.to_owned()),
        );
    }
    Value::Dict(map)
}

pub fn send_package_file(
    id: &str,
    path: &[String],
    last_modified: Option<&str>,
    data: &[u8],
) -> Value {
    let mut map = BTreeMap::new();
    map.insert("id".to_owned(), Value::Str(id.to_owned()));
    map.insert(
        "relativePathComponents".to_owned(),
        Value::Array(path.iter().cloned().map(Value::Str).collect()),
    );
    if let Some(last_modified) = last_modified {
        map.insert(
            "lastModified".to_owned(),
            Value::Str(last_modified.to_owned()),
        );
    }
    map.insert("data".to_owned(), Value::Bytes(data.to_vec()));
    Value::Dict(map)
}

pub fn delete_package_file(id: &str, path: &[String]) -> Value {
    Value::dict([
        ("id", Value::Str(id.to_owned())),
        (
            "relativePathComponents",
            Value::Array(path.iter().cloned().map(Value::Str).collect()),
        ),
    ])
}

pub fn mount_guest_tools(id: &str) -> Value {
    Value::dict([("id", Value::Str(id.to_owned()))])
}

pub fn start_virtual_machine(id: &str, options: i64) -> Value {
    Value::dict([
        ("id", Value::Str(id.to_owned())),
        ("options", Value::Int(options)),
    ])
}

pub fn stop_virtual_machine(id: &str, method: i64) -> Value {
    Value::dict([
        ("id", Value::Str(id.to_owned())),
        ("method", Value::Int(method)),
    ])
}

pub fn virtual_machine_id(id: &str) -> Value {
    Value::dict([("id", Value::Str(id.to_owned()))])
}

pub fn snapshot(id: &str, name: Option<&str>) -> Value {
    let mut map = BTreeMap::new();
    map.insert("id".to_owned(), Value::Str(id.to_owned()));
    if let Some(name) = name {
        map.insert("name".to_owned(), Value::Str(name.to_owned()));
    }
    Value::Dict(map)
}

pub fn change_pointer_type(id: &str, tablet: bool) -> Value {
    Value::dict([
        ("id", Value::Str(id.to_owned())),
        ("isTabletMode", Value::Bool(tablet)),
    ])
}

// ---------------------------------------------------------------------------
// Reply / incoming request schemas
// ---------------------------------------------------------------------------

pub mod schemas {
    use super::Type;

    pub static SERVER_INFORMATION: Type = Type::Struct(&[
        ("spicePortInternal", Type::Int),
        ("spicePortExternal", Type::Int),
        ("spiceHostExternal", Type::Str),
        ("spicePublicKey", Type::Bytes),
        ("spicePassword", Type::Str),
    ]);

    pub static VM_INFORMATION: Type = Type::Struct(&[
        ("id", Type::Str),
        ("name", Type::Str),
        ("path", Type::Str),
        ("isShortcut", Type::Bool),
        ("isSuspended", Type::Bool),
        ("isTakeoverAllowed", Type::Bool),
        ("backend", Type::Enum),
        ("state", Type::Enum),
        ("mountedDrives", Type::MapStrStr),
    ]);

    pub static VM_INFORMATION_LIST: Type = Type::Array(&VM_INFORMATION);

    pub static SERVER_HANDSHAKE_REPLY: Type = Type::Struct(&[
        ("version", Type::Int),
        ("isAuthenticated", Type::Bool),
        ("capabilities", Type::Int),
        ("model", Type::Str),
    ]);

    pub static CLIENT_HANDSHAKE_REPLY: Type =
        Type::Struct(&[("version", Type::Int), ("capabilities", Type::Int)]);

    pub static LIST_VMS_REPLY: Type = Type::Struct(&[("ids", Type::Array(&Type::Str))]);

    pub static GET_VM_INFO_REPLY: Type = Type::Struct(&[("informations", VM_INFORMATION_LIST)]);

    pub static START_VM_REPLY: Type = Type::Struct(&[("serverInfo", SERVER_INFORMATION)]);

    pub static GET_PACKAGE_SIZE_REPLY: Type = Type::Struct(&[("size", Type::Int)]);

    pub static GET_PACKAGE_FILE_REPLY: Type =
        Type::Struct(&[("data", Type::Bytes), ("lastModified", Type::Str)]);

    pub static EMPTY_REPLY: Type = Type::Struct(&[]);

    pub static CLIENT_HANDSHAKE_REQUEST: Type = Type::Struct(&[("version", Type::Int)]);

    pub static LIST_HAS_CHANGED_REQUEST: Type = Type::Struct(&[("ids", Type::Array(&Type::Str))]);

    pub static QEMU_CONFIGURATION_HAS_CHANGED_REQUEST: Type =
        Type::Struct(&[("id", Type::Str), ("configuration", Type::Skip)]);

    pub static MOUNTED_DRIVES_HAS_CHANGED_REQUEST: Type =
        Type::Struct(&[("id", Type::Str), ("mountedDrives", Type::MapStrStr)]);

    pub static VM_DID_TRANSITION_REQUEST: Type = Type::Struct(&[
        ("id", Type::Str),
        ("state", Type::Enum),
        ("isTakeoverAllowed", Type::Bool),
    ]);

    pub static VM_DID_ERROR_REQUEST: Type =
        Type::Struct(&[("id", Type::Str), ("errorMessage", Type::Str)]);
}
