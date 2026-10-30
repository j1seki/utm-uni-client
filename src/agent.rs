//! Shared UTM Remote connection and VM event fan-out.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use serde::Serialize;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, warn};

use crate::error::{Error, Result};
use crate::utm::peer::ServerEvent;
use crate::utm::types::{StopMethod, VmInfo, VmState};
use crate::utm::UtmClient;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    Vms { vms: Vec<VmInfo> },
    Vm { vm: VmInfo },
    Error { id: String, message: String },
    Disconnected { reason: String },
}

pub struct Agent {
    pub profile_id: String,
    pub client: Arc<UtmClient>,
    vms: RwLock<Vec<VmInfo>>,
    pub events: broadcast::Sender<Arc<AgentEvent>>,
    pub connected_at: Instant,
    closed: AtomicBool,
}

impl Agent {
    pub fn spawn(
        profile_id: String,
        client: Arc<UtmClient>,
        mut server_events: mpsc::UnboundedReceiver<ServerEvent>,
    ) -> Arc<Self> {
        let (events, _) = broadcast::channel::<Arc<AgentEvent>>(128);
        let agent = Arc::new(Self {
            profile_id,
            client,
            vms: RwLock::new(Vec::new()),
            events: events.clone(),
            connected_at: Instant::now(),
            closed: AtomicBool::new(false),
        });

        let event_agent = Arc::clone(&agent);
        tokio::spawn(async move {
            while let Some(event) = server_events.recv().await {
                event_agent.handle_server_event(event).await;
            }
        });

        agent
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<AgentEvent>> {
        self.events.subscribe()
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst) || self.client.peer.is_closed()
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.client.close();
    }

    pub fn vms(&self) -> Vec<VmInfo> {
        self.vms.read().expect("vms lock poisoned").clone()
    }

    pub async fn refresh(&self) -> Result<Vec<VmInfo>> {
        let vms = self.client.list_vms().await?;
        *self.vms.write().expect("vms lock poisoned") = vms.clone();
        let _ = self
            .events
            .send(Arc::new(AgentEvent::Vms { vms: vms.clone() }));
        Ok(vms)
    }

    pub async fn refresh_if_empty(&self) -> Result<Vec<VmInfo>> {
        let current = self.vms();
        if current.is_empty() {
            self.refresh().await
        } else {
            Ok(current)
        }
    }

    async fn handle_server_event(&self, event: ServerEvent) {
        match event {
            ServerEvent::ListChanged(_) => {
                if let Err(error) = self.refresh().await {
                    debug!("failed to refresh VM list: {error}");
                }
            }
            ServerEvent::VmTransition {
                id,
                state,
                is_takeover_allowed,
            } => {
                let updated = {
                    let mut vms = self.vms.write().expect("vms lock poisoned");
                    if let Some(vm) = vms.iter_mut().find(|vm| vm.id == id) {
                        vm.state = VmState::from_name(&state).unwrap_or(vm.state);
                        vm.is_takeover_allowed = is_takeover_allowed;
                        Some(vm.clone())
                    } else {
                        None
                    }
                };
                match updated {
                    Some(vm) => {
                        let _ = self.events.send(Arc::new(AgentEvent::Vm { vm }));
                    }
                    None => {
                        if let Err(error) = self.refresh().await {
                            debug!("failed to refresh after transition: {error}");
                        }
                    }
                }
            }
            ServerEvent::MountedDrivesChanged(id, drives) => {
                let updated = {
                    let mut vms = self.vms.write().expect("vms lock poisoned");
                    vms.iter_mut().find(|vm| vm.id == id).map(|vm| {
                        vm.mounted_drives = drives;
                        vm.clone()
                    })
                };
                if let Some(vm) = updated {
                    let _ = self.events.send(Arc::new(AgentEvent::Vm { vm }));
                }
            }
            ServerEvent::VmError { id, message } => {
                warn!("VM {id} error: {message}");
                let _ = self
                    .events
                    .send(Arc::new(AgentEvent::Error { id, message }));
            }
            ServerEvent::QemuConfigurationChanged(_) => {}
            ServerEvent::Disconnected(reason) => {
                self.closed.store(true, Ordering::SeqCst);
                let _ = self
                    .events
                    .send(Arc::new(AgentEvent::Disconnected { reason }));
            }
        }
    }

    pub async fn start(&self, id: &str) -> Result<()> {
        let _ = self
            .client
            .start_virtual_machine(id, crate::utm::types::StartOptions::NONE)
            .await?;
        let _ = self.refresh().await;
        Ok(())
    }

    pub async fn stop(&self, id: &str, method: StopMethod) -> Result<()> {
        self.client.stop_virtual_machine(id, method).await?;
        let _ = self.refresh().await;
        Ok(())
    }

    pub async fn restart(&self, id: &str) -> Result<()> {
        self.client.restart_virtual_machine(id).await?;
        let _ = self.refresh().await;
        Ok(())
    }

    pub async fn pause(&self, id: &str) -> Result<()> {
        self.client.pause_virtual_machine(id).await?;
        let _ = self.refresh().await;
        Ok(())
    }

    pub async fn resume(&self, id: &str) -> Result<()> {
        self.client.resume_virtual_machine(id).await?;
        let _ = self.refresh().await;
        Ok(())
    }

    pub async fn mount_guest_tools(&self, id: &str) -> Result<()> {
        self.client.mount_guest_tools(id).await?;
        Ok(())
    }

    pub async fn change_pointer(&self, id: &str, tablet: bool) -> Result<()> {
        self.client.change_pointer_type(id, tablet).await
    }
}

/// Translate a user-facing action name.
pub async fn perform_action(agent: &Agent, id: &str, action: &str) -> Result<()> {
    match action {
        "start" => agent.start(id).await,
        "stop" => agent.stop(id, StopMethod::Request).await,
        "force-stop" | "force_stop" => agent.stop(id, StopMethod::Force).await,
        "kill" => agent.stop(id, StopMethod::Kill).await,
        "restart" => agent.restart(id).await,
        "pause" => agent.pause(id).await,
        "resume" => agent.resume(id).await,
        "mount-guest-tools" | "mount_guest_tools" => agent.mount_guest_tools(id).await,
        other => Err(Error::BadRequest(format!("unknown VM action {other:?}"))),
    }
}
