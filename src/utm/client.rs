use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_rustls::TlsConnector;

use crate::cert::{
    connection_fingerprint, format_fingerprint, server_certificate_fingerprint, ClientIdentity,
};
use crate::codable::{decode, Value};
use crate::error::{Error, Result};
use crate::utm::messages::{self, schemas, server};
use crate::utm::peer::{Peer, ServerEvent};
use crate::utm::types::{ServerInfo, StartOptions, StopMethod, VmInfo};

pub use crate::cert::ClientIdentity as Identity;

#[derive(Debug, Clone)]
pub struct UtmConnectOptions {
    pub host: String,
    pub port: u16,
    pub password: Option<String>,
    /// If set, the connection fingerprint must match this value.
    pub expected_fingerprint: Option<[u8; 32]>,
    /// If true, accept an unknown server certificate fingerprint.  This is
    /// only used after the user explicitly confirms the fingerprint in the UI.
    pub allow_untrusted: bool,
}

pub struct UtmClient {
    pub peer: Arc<Peer>,
    pub host: String,
    pub port: u16,
    pub server_fingerprint: [u8; 32],
    pub client_fingerprint: [u8; 32],
    pub connection_fingerprint: [u8; 32],
    pub model: String,
    pub capabilities: i64,
}

impl std::fmt::Debug for UtmClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UtmClient")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("model", &self.model)
            .field(
                "connection_fingerprint",
                &self.connection_fingerprint_string(),
            )
            .finish()
    }
}

impl UtmClient {
    pub async fn connect(
        identity: Arc<ClientIdentity>,
        options: UtmConnectOptions,
    ) -> Result<(Self, mpsc::UnboundedReceiver<ServerEvent>)> {
        let tcp = tokio::time::timeout(
            Duration::from_secs(15),
            crate::net::connect(&options.host, options.port),
        )
        .await
        .map_err(|_| Error::Timeout)?
        .map_err(Error::Io)?;
        tcp.set_nodelay(true).ok();

        let tls_config = crate::cert::utm_client_config(&identity)?;
        let connector = TlsConnector::from(Arc::new(tls_config));
        let server_name = crate::net::tls_server_name(&options.host)
            .ok_or_else(|| Error::BadRequest(format!("invalid host name {:?}", options.host)))?;
        let tls =
            tokio::time::timeout(Duration::from_secs(15), connector.connect(server_name, tcp))
                .await
                .map_err(|_| Error::Timeout)?
                .map_err(Error::Io)?;

        let server_fingerprint = server_certificate_fingerprint(&tls)?;
        let client_fingerprint = identity.fingerprint;
        let connection = connection_fingerprint(&server_fingerprint, &client_fingerprint);

        if let Some(expected) = options.expected_fingerprint {
            if expected != connection {
                return Err(Error::FingerprintMismatch {
                    expected: format_fingerprint(&expected),
                    actual: format_fingerprint(&connection),
                });
            }
        } else if !options.allow_untrusted {
            return Err(Error::Untrusted {
                fingerprint: format_fingerprint(&connection),
            });
        }

        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let peer = Peer::spawn(tls, event_tx);
        peer.set_trusted();

        let reply = peer
            .request(
                server::HANDSHAKE,
                &messages::handshake(1, options.password.as_deref()),
            )
            .await?;
        let reply = decode(&reply, schemas::SERVER_HANDSHAKE_REPLY)?;
        let version = reply.get("version").and_then(Value::as_i64).unwrap_or(0);
        if version != 1 {
            peer.close();
            return Err(Error::Protocol(format!(
                "server interface version {version} does not match client version 1"
            )));
        }
        let authenticated = reply
            .get("isAuthenticated")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !authenticated {
            peer.close();
            return Err(if options.password.as_deref().unwrap_or("").is_empty() {
                Error::PasswordRequired
            } else {
                Error::InvalidPassword
            });
        }
        let capabilities = reply
            .get("capabilities")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let model = reply
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();

        Ok((
            Self {
                peer,
                host: options.host,
                port: options.port,
                server_fingerprint,
                client_fingerprint,
                connection_fingerprint: connection,
                model,
                capabilities,
            },
            event_rx,
        ))
    }

    pub fn connection_fingerprint_string(&self) -> String {
        format_fingerprint(&self.connection_fingerprint)
    }

    pub fn server_fingerprint_string(&self) -> String {
        format_fingerprint(&self.server_fingerprint)
    }

    pub fn client_fingerprint_string(&self) -> String {
        format_fingerprint(&self.client_fingerprint)
    }

    pub fn close(&self) {
        self.peer.close();
    }

    async fn request(&self, id: u8, body: &Value, schema: crate::codable::Type) -> Result<Value> {
        let reply = self.peer.request(id, body).await?;
        decode(&reply, schema)
    }

    async fn request_empty(&self, id: u8, body: &Value) -> Result<()> {
        let _ = self.request(id, body, schemas::EMPTY_REPLY).await?;
        Ok(())
    }

    pub async fn list_virtual_machines(&self) -> Result<Vec<String>> {
        let reply = self
            .request(
                server::LIST_VIRTUAL_MACHINES,
                &messages::list_virtual_machines(),
                schemas::LIST_VMS_REPLY,
            )
            .await?;
        Ok(reply
            .get("ids")
            .and_then(Value::as_array)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| id.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn get_virtual_machine_information(&self, ids: &[String]) -> Result<Vec<VmInfo>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let reply = self
            .request(
                server::GET_VIRTUAL_MACHINE_INFORMATION,
                &messages::get_virtual_machine_information(ids),
                schemas::GET_VM_INFO_REPLY,
            )
            .await?;
        Ok(reply
            .get("informations")
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(VmInfo::from_value).collect())
            .unwrap_or_default())
    }

    pub async fn list_vms(&self) -> Result<Vec<VmInfo>> {
        let ids = self.list_virtual_machines().await?;
        self.get_virtual_machine_information(&ids).await
    }

    pub async fn reorder_virtual_machines(&self, ids: &[String], offset: i64) -> Result<()> {
        self.request_empty(
            server::REORDER_VIRTUAL_MACHINES,
            &messages::reorder_virtual_machines(ids, offset),
        )
        .await
    }

    pub async fn start_virtual_machine(
        &self,
        id: &str,
        options: StartOptions,
    ) -> Result<ServerInfo> {
        let reply = self
            .request(
                server::START_VIRTUAL_MACHINE,
                &messages::start_virtual_machine(id, options.0),
                schemas::START_VM_REPLY,
            )
            .await?;
        reply
            .get("serverInfo")
            .and_then(ServerInfo::from_value)
            .ok_or_else(|| Error::Protocol("start reply has no serverInfo".into()))
    }

    pub async fn stop_virtual_machine(&self, id: &str, method: StopMethod) -> Result<()> {
        self.request_empty(
            server::STOP_VIRTUAL_MACHINE,
            &messages::stop_virtual_machine(id, method as i64),
        )
        .await
    }

    pub async fn restart_virtual_machine(&self, id: &str) -> Result<()> {
        self.request_empty(
            server::RESTART_VIRTUAL_MACHINE,
            &messages::virtual_machine_id(id),
        )
        .await
    }

    pub async fn pause_virtual_machine(&self, id: &str) -> Result<()> {
        self.request_empty(
            server::PAUSE_VIRTUAL_MACHINE,
            &messages::virtual_machine_id(id),
        )
        .await
    }

    pub async fn resume_virtual_machine(&self, id: &str) -> Result<()> {
        self.request_empty(
            server::RESUME_VIRTUAL_MACHINE,
            &messages::virtual_machine_id(id),
        )
        .await
    }

    pub async fn save_snapshot(&self, id: &str, name: Option<&str>) -> Result<()> {
        self.request_empty(server::SAVE_SNAPSHOT, &messages::snapshot(id, name))
            .await
    }

    pub async fn delete_snapshot(&self, id: &str, name: Option<&str>) -> Result<()> {
        self.request_empty(server::DELETE_SNAPSHOT, &messages::snapshot(id, name))
            .await
    }

    pub async fn restore_snapshot(&self, id: &str, name: Option<&str>) -> Result<()> {
        self.request_empty(server::RESTORE_SNAPSHOT, &messages::snapshot(id, name))
            .await
    }

    pub async fn mount_guest_tools(&self, id: &str) -> Result<()> {
        self.request_empty(server::MOUNT_GUEST_TOOLS, &messages::mount_guest_tools(id))
            .await
    }

    pub async fn change_pointer_type(&self, id: &str, tablet: bool) -> Result<()> {
        self.request_empty(
            server::CHANGE_POINTER_TYPE,
            &messages::change_pointer_type(id, tablet),
        )
        .await
    }

    pub async fn get_package_size(&self, id: &str) -> Result<i64> {
        let reply = self
            .request(
                server::GET_PACKAGE_SIZE,
                &messages::get_package_size(id),
                schemas::GET_PACKAGE_SIZE_REPLY,
            )
            .await?;
        Ok(reply.get("size").and_then(Value::as_i64).unwrap_or(0))
    }

    pub async fn get_package_file(
        &self,
        id: &str,
        path: &[String],
        last_modified: Option<&str>,
    ) -> Result<(Vec<u8>, Option<String>)> {
        let reply = self
            .request(
                server::GET_PACKAGE_FILE,
                &messages::get_package_file(id, path, last_modified),
                schemas::GET_PACKAGE_FILE_REPLY,
            )
            .await?;
        Ok((
            reply
                .get("data")
                .and_then(Value::as_bytes)
                .unwrap_or_default()
                .to_vec(),
            reply
                .get("lastModified")
                .and_then(Value::as_str)
                .map(str::to_owned),
        ))
    }

    pub async fn send_package_file(
        &self,
        id: &str,
        path: &[String],
        last_modified: Option<&str>,
        data: &[u8],
    ) -> Result<()> {
        self.request_empty(
            server::SEND_PACKAGE_FILE,
            &messages::send_package_file(id, path, last_modified, data),
        )
        .await
    }

    pub async fn delete_package_file(&self, id: &str, path: &[String]) -> Result<()> {
        self.request_empty(
            server::DELETE_PACKAGE_FILE,
            &messages::delete_package_file(id, path),
        )
        .await
    }

    /// QEMU configuration replies contain a large, versioned nested Codable
    /// structure.  The browser client does not need it, so this method returns
    /// the raw Codable payload for tooling/tests.
    pub async fn get_qemu_configuration_raw(&self, id: &str) -> Result<Vec<u8>> {
        self.peer
            .request(
                server::GET_QEMU_CONFIGURATION,
                &messages::get_qemu_configuration(id),
            )
            .await
    }
}
