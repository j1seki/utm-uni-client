//! One live SPICE display pipeline per (profile, virtual machine).

use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::error::{Error, Result};
use crate::spice::display::SharedSurfaces;
use crate::spice::display::{DisplayRenderer, DisplayUpdate};
use crate::spice::inputs::InputCommand;
use crate::spice::link::{connect_channel, LinkOptions};
use crate::spice::main::{connect_main, MainInfo};
use crate::spice::surface::{Canvas, Rect};
use crate::spice::{
    common_caps, display_channel_caps, initialize_display, inputs_channel_caps,
    run_display_channel, run_inputs_channel,
};
use crate::utm::types::{StartOptions, VmInfo};
use crate::utm::UtmClient;

#[derive(Debug, Clone)]
pub struct Frame {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub surface_w: u32,
    pub surface_h: u32,
    pub seq: u64,
    pub pixels: Arc<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub enum DisplayEvent {
    Resize { width: u32, height: u32 },
    Frame(Arc<Frame>),
    MouseMode(u16),
    Notice(String),
    Error(String),
    Closed,
}

pub struct DisplaySession {
    pub key: String,
    pub profile_id: String,
    pub vm_id: String,
    pub vm_name: String,
    pub surfaces: SharedSurfaces,
    pub width: AtomicU32,
    pub height: AtomicU32,
    pub mouse_mode: AtomicU16,
    pub viewers: AtomicUsize,
    pub last_active: AtomicU64,
    pub closed: AtomicBool,
    pub events: broadcast::Sender<Arc<DisplayEvent>>,
    pub input_tx: mpsc::Sender<InputCommand>,
    pub tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl DisplaySession {
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<DisplayEvent>> {
        self.events.subscribe()
    }

    pub fn touch(&self) {
        self.last_active.store(now_millis(), Ordering::Relaxed);
    }

    pub fn idle_for(&self) -> Duration {
        Duration::from_millis(now_millis().saturating_sub(self.last_active.load(Ordering::Relaxed)))
    }

    pub fn full_frame(&self) -> Option<Arc<Frame>> {
        snapshot(&self.surfaces, Rect::new(0, 0, i32::MAX, i32::MAX), 0)
    }

    pub fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = self.events.send(Arc::new(DisplayEvent::Closed));
        for task in self.tasks.lock().expect("task lock poisoned").drain(..) {
            task.abort();
        }
    }
}

impl Drop for DisplaySession {
    fn drop(&mut self) {
        self.close();
    }
}

pub struct CreateDisplay {
    pub key: String,
    pub profile_id: String,
    pub vm_id: String,
    pub vm_name: String,
    pub client: Arc<UtmClient>,
    pub server_host: String,
}

pub async fn create_display(request: CreateDisplay) -> Result<Arc<DisplaySession>> {
    let CreateDisplay {
        key,
        profile_id,
        vm_id,
        vm_name,
        client,
        server_host,
    } = request;

    info!("starting SPICE display for VM {vm_id}");
    let server_info = client
        .start_virtual_machine(&vm_id, StartOptions::NONE)
        .await?;
    let (host, port) = server_info
        .endpoint(&server_host)
        .ok_or_else(|| Error::Spice("UTM server did not advertise a SPICE endpoint".into()))?;
    if server_info.spice_public_key.is_empty() {
        return Err(Error::Spice(
            "UTM server did not provide a SPICE public key".into(),
        ));
    }

    let common = common_caps();
    let main = connect_main(LinkOptions {
        host: host.clone(),
        port,
        password: server_info.spice_password.clone(),
        expected_spki: server_info.spice_public_key.clone(),
        connection_id: 0,
        channel_type: crate::spice::protocol::CHANNEL_MAIN,
        channel_id: 0,
        common_caps: common.clone(),
        channel_caps: Vec::new(),
        timeout: Duration::from_secs(15),
    })
    .await?;
    let session_id = main.info.session_id;
    if session_id == 0 {
        return Err(Error::Spice(
            "SPICE main channel returned session id 0".into(),
        ));
    }
    debug!("SPICE session id {session_id}");

    let display = connect_channel(LinkOptions {
        host: host.clone(),
        port,
        password: server_info.spice_password.clone(),
        expected_spki: server_info.spice_public_key.clone(),
        connection_id: session_id,
        channel_type: crate::spice::protocol::CHANNEL_DISPLAY,
        channel_id: 0,
        common_caps: common.clone(),
        channel_caps: display_channel_caps(),
        timeout: Duration::from_secs(15),
    })
    .await?;
    initialize_display(&display.writer, true).await?;

    let renderer = DisplayRenderer::new();
    let surfaces = Arc::clone(&renderer.surfaces);
    let (updates_tx, updates_rx) = mpsc::channel::<DisplayUpdate>(2048);
    let display_task = tokio::spawn(run_display_channel(
        display.reader,
        Arc::clone(&display.writer),
        renderer,
        updates_tx,
    ));

    let inputs = connect_channel(LinkOptions {
        host: host.clone(),
        port,
        password: server_info.spice_password.clone(),
        expected_spki: server_info.spice_public_key.clone(),
        connection_id: session_id,
        channel_type: crate::spice::protocol::CHANNEL_INPUTS,
        channel_id: 0,
        common_caps: common,
        channel_caps: inputs_channel_caps(),
        timeout: Duration::from_secs(15),
    })
    .await?;
    let inputs_task = tokio::spawn(run_inputs_channel(
        inputs.reader,
        Arc::clone(&inputs.writer),
    ));

    let (input_tx, mut input_rx) = mpsc::channel::<InputCommand>(512);
    let input_writer = Arc::clone(&inputs.writer);
    let input_task = tokio::spawn(async move {
        while let Some(command) = input_rx.recv().await {
            if let Err(error) = command.send(&input_writer).await {
                debug!("input command failed: {error}");
                break;
            }
        }
    });

    let (events, _) = broadcast::channel::<Arc<DisplayEvent>>(256);
    let session = Arc::new(DisplaySession {
        key,
        profile_id,
        vm_id: vm_id.clone(),
        vm_name,
        surfaces,
        width: AtomicU32::new(0),
        height: AtomicU32::new(0),
        mouse_mode: AtomicU16::new(0),
        viewers: AtomicUsize::new(0),
        last_active: AtomicU64::new(now_millis()),
        closed: AtomicBool::new(false),
        events,
        input_tx,
        tasks: Mutex::new(Vec::new()),
    });

    {
        let session_for_sink = Arc::clone(&session);
        let task = tokio::spawn(frame_sink(updates_rx, session_for_sink));
        session.tasks.lock().expect("task lock poisoned").push(task);
    }
    {
        let session_for_mode = Arc::clone(&session);
        let mut mouse_mode = main.mouse_mode.clone();
        let initial = main.info.current_mouse_mode;
        session.mouse_mode.store(initial, Ordering::Relaxed);
        let task = tokio::spawn(async move {
            loop {
                if mouse_mode.changed().await.is_err() {
                    break;
                }
                let info: MainInfo = mouse_mode.borrow().clone();
                session_for_mode
                    .mouse_mode
                    .store(info.current_mouse_mode, Ordering::Relaxed);
                let _ = session_for_mode
                    .events
                    .send(Arc::new(DisplayEvent::MouseMode(info.current_mouse_mode)));
            }
        });
        session.tasks.lock().expect("task lock poisoned").push(task);
    }
    {
        let mut tasks = session.tasks.lock().expect("task lock poisoned");
        tasks.push(display_task);
        tasks.push(inputs_task);
        tasks.push(input_task);
        tasks.push(main.task);
    }
    session.touch();
    info!("SPICE display ready for VM {vm_id}");
    Ok(session)
}

async fn frame_sink(mut updates: mpsc::Receiver<DisplayUpdate>, session: Arc<DisplaySession>) {
    let mut pending: Option<Rect> = None;
    let mut seq = 0u64;
    loop {
        let Some(update) = updates.recv().await else {
            break;
        };
        apply_update(&session, update, &mut pending);
        // Coalesce bursts of draw operations into a single WebSocket frame.
        let deadline = tokio::time::Instant::now() + Duration::from_millis(8);
        while let Ok(Some(update)) = tokio::time::timeout_at(deadline, updates.recv()).await {
            apply_update(&session, update, &mut pending);
        }
        if let Some(rect) = pending.take() {
            if let Some(frame) = snapshot(&session.surfaces, rect, seq) {
                seq = seq.wrapping_add(1);
                session.touch();
                let _ = session.events.send(Arc::new(DisplayEvent::Frame(frame)));
            }
        }
    }
}

fn apply_update(session: &Arc<DisplaySession>, update: DisplayUpdate, pending: &mut Option<Rect>) {
    match update {
        DisplayUpdate::Resize { width, height } => {
            session.width.store(width, Ordering::Relaxed);
            session.height.store(height, Ordering::Relaxed);
            let _ = session
                .events
                .send(Arc::new(DisplayEvent::Resize { width, height }));
            *pending = Some(Rect::new(0, 0, width as i32, height as i32));
        }
        DisplayUpdate::Dirty(rect) => {
            *pending = Some(match pending.take() {
                Some(current) => current.union(rect),
                None => rect,
            });
        }
        DisplayUpdate::Mark(_) => {}
        DisplayUpdate::Unsupported(message) => {
            warn!("SPICE display: {message}");
            let _ = session.events.send(Arc::new(DisplayEvent::Notice(message)));
        }
    }
}

fn snapshot(surfaces: &SharedSurfaces, rect: Rect, seq: u64) -> Option<Arc<Frame>> {
    let surfaces = surfaces.read().ok()?;
    let primary = surfaces.get(&0)?;
    let canvas: std::sync::RwLockReadGuard<'_, Canvas> = primary.read().ok()?;
    let rect = rect.clamp(canvas.width, canvas.height);
    if rect.is_empty() {
        return None;
    }
    let width = rect.w as usize;
    let height = rect.h as usize;
    let mut pixels = Vec::with_capacity(width * height * 4);
    for row in 0..height {
        let y = rect.y as usize + row;
        let start = (y * canvas.width as usize + rect.x as usize) * 4;
        let end = start + width * 4;
        if end > canvas.pixels.len() {
            return None;
        }
        pixels.extend_from_slice(&canvas.pixels[start..end]);
    }
    Some(Arc::new(Frame {
        x: rect.x as u32,
        y: rect.y as u32,
        w: rect.w as u32,
        h: rect.h as u32,
        surface_w: canvas.width,
        surface_h: canvas.height,
        seq,
        pixels: Arc::new(pixels),
    }))
}

pub fn vm_name(vms: &[VmInfo], id: &str) -> String {
    vms.iter()
        .find(|vm| vm.id == id)
        .map(|vm| vm.name.clone())
        .unwrap_or_else(|| id.to_owned())
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
