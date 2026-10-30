//! HTTP API and WebSocket endpoints.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::agent::{perform_action, Agent, AgentEvent};
use crate::cert::{format_fingerprint, parse_fingerprint, ClientIdentity};
use crate::config::AppConfig;
use crate::display_session::{create_display, DisplayEvent, DisplaySession, Frame};
use crate::error::Error;
use crate::profiles::{new_id, Profile, ProfileStore, ProfileView};
use crate::spice::inputs::InputCommand;
use crate::spice::keymap::mouse_button_mask;
use crate::spice::protocol::MOUSE_MODE_CLIENT;
use crate::utm::types::{StartOptions, StopMethod, VmInfo, SERVER_PORT_DEFAULT};
use crate::utm::{UtmClient, UtmConnectOptions};

const INDEX_HTML: &str = include_str!("../web/dist/index.html");
const APP_JS: &str = include_str!("../web/dist/app.js");
const APP_CSS: &str = include_str!("../web/dist/app.css");

#[derive(Clone)]
pub struct AppState {
    pub config: AppConfig,
    pub identity: Arc<ClientIdentity>,
    pub profiles: Arc<ProfileStore>,
    agents: Arc<Mutex<HashMap<String, Arc<Agent>>>>,
    displays: Arc<Mutex<HashMap<String, Arc<DisplaySession>>>>,
    connect_lock: Arc<Mutex<()>>,
    display_lock: Arc<Mutex<()>>,
    pub started: Instant,
}

impl AppState {
    pub fn new(
        config: AppConfig,
        identity: Arc<ClientIdentity>,
        profiles: Arc<ProfileStore>,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            identity,
            profiles,
            agents: Arc::new(Mutex::new(HashMap::new())),
            displays: Arc::new(Mutex::new(HashMap::new())),
            connect_lock: Arc::new(Mutex::new(())),
            display_lock: Arc::new(Mutex::new(())),
            started: Instant::now(),
        })
    }

    pub async fn get_agent(&self, profile_id: &str) -> std::result::Result<Arc<Agent>, ApiError> {
        let profile = self
            .profiles
            .get(profile_id)
            .ok_or_else(|| ApiError::not_found("profile not found"))?;

        {
            let mut agents = self.agents.lock().await;
            if let Some(agent) = agents.get(profile_id) {
                if !agent.is_closed() {
                    return Ok(Arc::clone(agent));
                }
                agents.remove(profile_id);
            }
        }

        let _guard = self.connect_lock.lock().await;
        {
            let agents = self.agents.lock().await;
            if let Some(agent) = agents.get(profile_id) {
                if !agent.is_closed() {
                    return Ok(Arc::clone(agent));
                }
            }
        }

        let expected = profile
            .fingerprint
            .as_deref()
            .and_then(|value| parse_fingerprint(value).ok());
        let allow_untrusted = self.config.allow_untrusted;

        let options = UtmConnectOptions {
            host: profile.host.clone(),
            port: profile.port,
            password: profile.password.clone(),
            expected_fingerprint: expected,
            allow_untrusted,
        };
        info!(
            "connecting to UTM server {}:{} (profile {})",
            profile.host, profile.port, profile.id
        );
        let result = UtmClient::connect(Arc::clone(&self.identity), options).await;
        let (client, events) = match result {
            Ok(value) => value,
            Err(Error::Untrusted { fingerprint }) => {
                let _ = self.profiles.update(&profile.id, |p| {
                    p.fingerprint = Some(fingerprint.clone());
                    p.trusted = false;
                });
                return Err(ApiError::untrusted(fingerprint));
            }
            Err(error) => return Err(error.into()),
        };
        let client = Arc::new(client);
        let fingerprint = client.connection_fingerprint_string();
        let _ = self.profiles.update(&profile.id, |p| {
            p.fingerprint = Some(fingerprint);
            p.trusted = true;
            p.last_connected = Some(now_secs());
        });
        let agent = Agent::spawn(profile.id.clone(), Arc::clone(&client), events);
        if let Err(error) = agent.refresh().await {
            warn!("connected but failed to list VMs: {error}");
        }
        let mut agents = self.agents.lock().await;
        agents.insert(profile.id.clone(), Arc::clone(&agent));
        Ok(agent)
    }

    /// Close every upstream connection.  Used during shutdown.
    pub async fn disconnect_all(&self) {
        let agents: Vec<Arc<Agent>> = {
            let mut agents = self.agents.lock().await;
            agents.drain().map(|(_, agent)| agent).collect()
        };
        for agent in agents {
            agent.close();
        }
        let displays: Vec<Arc<DisplaySession>> = {
            let mut displays = self.displays.lock().await;
            displays.drain().map(|(_, display)| display).collect()
        };
        for display in displays {
            display.close();
        }
    }

    pub async fn disconnect_agent(&self, profile_id: &str) {
        let agent = {
            let mut agents = self.agents.lock().await;
            agents.remove(profile_id)
        };
        if let Some(agent) = agent {
            agent.close();
        }
        self.close_profile_displays(profile_id).await;
    }

    async fn close_profile_displays(&self, profile_id: &str) {
        let mut displays = self.displays.lock().await;
        let keys: Vec<String> = displays
            .iter()
            .filter(|(_, display)| display.profile_id == profile_id)
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            if let Some(display) = displays.remove(&key) {
                display.close();
            }
        }
    }

    pub async fn ensure_display(
        &self,
        profile_id: &str,
        vm_id: &str,
    ) -> std::result::Result<Arc<DisplaySession>, ApiError> {
        let key = format!("{profile_id}:{vm_id}");
        let _guard = self.display_lock.lock().await;
        {
            let mut displays = self.displays.lock().await;
            if let Some(display) = displays.get(&key) {
                if !display.closed.load(Ordering::SeqCst) {
                    return Ok(Arc::clone(display));
                }
                displays.remove(&key);
            }
        }
        {
            let displays = self.displays.lock().await;
            if displays.len() >= self.config.max_sessions {
                return Err(ApiError::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    "too_many_sessions",
                    format!(
                        "The limit of {} concurrent display sessions has been reached.",
                        self.config.max_sessions
                    ),
                ));
            }
        }
        let agent = self.get_agent(profile_id).await?;
        let vms = agent.refresh_if_empty().await?;
        let name = vms
            .iter()
            .find(|vm| vm.id == vm_id)
            .map(|vm| vm.name.clone())
            .unwrap_or_else(|| vm_id.to_owned());
        let server_host = agent.client.host.clone();
        let session = create_display(crate::display_session::CreateDisplay {
            key: key.clone(),
            profile_id: profile_id.to_owned(),
            vm_id: vm_id.to_owned(),
            vm_name: name,
            client: Arc::clone(&agent.client),
            server_host,
        })
        .await?;
        let mut displays = self.displays.lock().await;
        displays.insert(key, Arc::clone(&session));
        Ok(session)
    }

    pub async fn close_display(&self, profile_id: &str, vm_id: &str) {
        let key = format!("{profile_id}:{vm_id}");
        let display = {
            let mut displays = self.displays.lock().await;
            displays.remove(&key)
        };
        if let Some(display) = display {
            display.close();
        }
    }

    pub async fn reap_displays(&self) {
        let timeout = self.config.session_idle_timeout;
        let mut expired = Vec::new();
        {
            let mut displays = self.displays.lock().await;
            for (key, display) in displays.iter() {
                let idle = display.idle_for();
                let viewers = display.viewers.load(Ordering::Relaxed);
                if display.closed.load(Ordering::SeqCst) || (viewers == 0 && idle > timeout) {
                    expired.push(key.clone());
                }
            }
            for key in &expired {
                if let Some(display) = displays.remove(key) {
                    display.close();
                }
            }
        }
        if !expired.is_empty() {
            debug!("closed {} idle display session(s)", expired.len());
        }
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/app.css", get(app_css))
        .route("/api/status", get(status))
        .route("/api/profiles", get(list_profiles).post(create_profile))
        .route(
            "/api/profiles/{id}",
            put(update_profile).delete(delete_profile),
        )
        .route("/api/profiles/{id}/trust", post(trust_profile))
        .route("/api/profiles/{id}/connect", post(connect_profile))
        .route("/api/profiles/{id}/disconnect", post(disconnect_profile))
        .route("/api/profiles/{id}/vms", get(list_vms))
        .route("/api/profiles/{id}/vms/{vm}/action", post(vm_action))
        .route("/api/profiles/{id}/events", get(agent_events))
        .route("/api/display/{profile}/{vm}/ws", get(display_ws))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Static frontend
// ---------------------------------------------------------------------------

async fn index() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'; connect-src 'self' ws: wss:",
            ),
        ],
        INDEX_HTML,
    )
        .into_response()
}

async fn app_js() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        APP_JS,
    )
        .into_response()
}

async fn app_css() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        APP_CSS,
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// API errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    fingerprint: Option<String>,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            fingerprint: None,
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    fn untrusted(fingerprint: String) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "untrusted",
            message: "The server certificate is not trusted yet.".into(),
            fingerprint: Some(fingerprint),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({
            "error": self.code,
            "message": self.message,
        });
        if let Some(fingerprint) = self.fingerprint {
            body["fingerprint"] = serde_json::Value::String(fingerprint);
        }
        (self.status, Json(body)).into_response()
    }
}

impl From<Error> for ApiError {
    fn from(error: Error) -> Self {
        match error {
            Error::Untrusted { fingerprint } => ApiError::untrusted(fingerprint),
            Error::FingerprintMismatch { expected, actual } => ApiError::new(
                StatusCode::CONFLICT,
                "fingerprint_mismatch",
                format!("Server fingerprint changed (expected {expected}, got {actual})"),
            ),
            Error::PasswordRequired => ApiError::new(
                StatusCode::UNAUTHORIZED,
                "password_required",
                "This server requires a password.",
            ),
            Error::InvalidPassword => ApiError::new(
                StatusCode::UNAUTHORIZED,
                "invalid_password",
                "The server rejected the password.",
            ),
            Error::Auth(message) => ApiError::new(StatusCode::UNAUTHORIZED, "auth_failed", message),
            Error::Timeout => ApiError::new(
                StatusCode::GATEWAY_TIMEOUT,
                "timeout",
                "The UTM server did not respond in time.",
            ),
            Error::Closed => ApiError::new(
                StatusCode::BAD_GATEWAY,
                "disconnected",
                "The UTM server connection was closed.",
            ),
            Error::BadRequest(message) => {
                ApiError::new(StatusCode::BAD_REQUEST, "bad_request", message)
            }
            Error::NotFound(message) => ApiError::not_found(message),
            Error::Unsupported(message) => {
                ApiError::new(StatusCode::NOT_IMPLEMENTED, "unsupported", message)
            }
            other => ApiError::new(StatusCode::BAD_GATEWAY, "upstream_error", other.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// Status and profiles
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct StatusResponse {
    version: &'static str,
    client_fingerprint: String,
    allow_untrusted: bool,
    profiles: usize,
    max_sessions: usize,
    session_idle_timeout_secs: u64,
    public_url: Option<String>,
    uptime_secs: u64,
}

async fn status(State(state): State<Arc<AppState>>) -> Json<StatusResponse> {
    Json(StatusResponse {
        version: env!("CARGO_PKG_VERSION"),
        client_fingerprint: state.identity.fingerprint_string(),
        allow_untrusted: state.config.allow_untrusted,
        profiles: state.profiles.list().len(),
        max_sessions: state.config.max_sessions,
        session_idle_timeout_secs: state.config.session_idle_timeout.as_secs(),
        public_url: state.config.public_url.clone(),
        uptime_secs: state.started.elapsed().as_secs(),
    })
}

async fn list_profiles(State(state): State<Arc<AppState>>) -> Json<Vec<ProfileView>> {
    Json(state.profiles.list())
}

#[derive(Deserialize)]
struct ProfileInput {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    fingerprint: Option<String>,
    #[serde(default)]
    trusted: Option<bool>,
}

async fn create_profile(
    State(state): State<Arc<AppState>>,
    Json(input): Json<ProfileInput>,
) -> std::result::Result<Json<ProfileView>, ApiError> {
    let host = input
        .host
        .as_deref()
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "bad_request", "host is required"))?
        .to_owned();
    let profile = Profile {
        id: new_id(),
        name: input.name.unwrap_or_default(),
        host,
        port: input.port.unwrap_or(SERVER_PORT_DEFAULT),
        fingerprint: input.fingerprint,
        password: input.password.filter(|password| !password.is_empty()),
        trusted: input.trusted.unwrap_or(false),
        last_connected: None,
    };
    Ok(Json(state.profiles.upsert(profile)?))
}

async fn update_profile(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(input): Json<ProfileInput>,
) -> std::result::Result<Json<ProfileView>, ApiError> {
    let existing = state
        .profiles
        .get(&id)
        .ok_or_else(|| ApiError::not_found("profile not found"))?;
    let mut updated = existing.clone();
    if let Some(name) = input.name {
        updated.name = name;
    }
    if let Some(host) = input.host {
        updated.host = host.trim().to_owned();
    }
    if let Some(port) = input.port {
        updated.port = port;
    }
    if let Some(password) = input.password {
        updated.password = if password.is_empty() {
            None
        } else {
            Some(password)
        };
    }
    if let Some(fingerprint) = input.fingerprint {
        updated.fingerprint = if fingerprint.is_empty() {
            None
        } else {
            Some(fingerprint)
        };
    }
    if let Some(trusted) = input.trusted {
        updated.trusted = trusted;
    }
    let needs_reconnect = updated.host != existing.host
        || updated.port != existing.port
        || updated.password != existing.password
        || updated.fingerprint != existing.fingerprint
        || updated.trusted != existing.trusted;
    if needs_reconnect {
        state.disconnect_agent(&id).await;
    }
    Ok(Json(state.profiles.upsert(updated)?))
}

async fn delete_profile(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    state.disconnect_agent(&id).await;
    if state.profiles.delete(&id)? {
        Ok(Json(json!({ "ok": true })))
    } else {
        Err(ApiError::not_found("profile not found"))
    }
}

#[derive(Deserialize)]
struct TrustInput {
    fingerprint: String,
}

async fn trust_profile(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(input): Json<TrustInput>,
) -> std::result::Result<Json<ProfileView>, ApiError> {
    let fingerprint = parse_fingerprint(&input.fingerprint)?;
    let formatted = format_fingerprint(&fingerprint);
    let view = state
        .profiles
        .update(&id, |profile| {
            profile.fingerprint = Some(formatted.clone());
            profile.trusted = true;
        })?
        .ok_or_else(|| ApiError::not_found("profile not found"))?;
    state.disconnect_agent(&id).await;
    Ok(Json(view))
}

#[derive(Serialize)]
struct ConnectResponse {
    profile: ProfileView,
    model: String,
    connection_fingerprint: String,
    server_fingerprint: String,
    client_fingerprint: String,
    vms: Vec<VmInfo>,
}

async fn connect_profile(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> std::result::Result<Json<ConnectResponse>, ApiError> {
    let agent = state.get_agent(&id).await?;
    let vms = agent.refresh().await?;
    let profile = state
        .profiles
        .list()
        .into_iter()
        .find(|profile| profile.id == id)
        .ok_or_else(|| ApiError::not_found("profile not found"))?;
    Ok(Json(ConnectResponse {
        profile,
        model: agent.client.model.clone(),
        connection_fingerprint: agent.client.connection_fingerprint_string(),
        server_fingerprint: agent.client.server_fingerprint_string(),
        client_fingerprint: agent.client.client_fingerprint_string(),
        vms,
    }))
}

async fn disconnect_profile(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    state.disconnect_agent(&id).await;
    Json(json!({ "ok": true }))
}

async fn list_vms(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> std::result::Result<Json<Vec<VmInfo>>, ApiError> {
    let agent = state.get_agent(&id).await?;
    Ok(Json(agent.refresh().await?))
}

#[derive(Deserialize)]
struct ActionInput {
    action: String,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    tablet: Option<bool>,
}

async fn vm_action(
    State(state): State<Arc<AppState>>,
    Path((profile_id, vm_id)): Path<(String, String)>,
    Json(input): Json<ActionInput>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let agent = state.get_agent(&profile_id).await?;
    let action = match (input.action.as_str(), input.method.as_deref()) {
        ("stop", Some("force")) => "force-stop",
        ("stop", Some("kill")) => "kill",
        (other, _) => other,
    };
    if action == "pointer" {
        agent
            .change_pointer(&vm_id, input.tablet.unwrap_or(true))
            .await?;
        return Ok(Json(json!({ "ok": true })));
    }
    if matches!(action, "stop" | "force-stop" | "kill" | "restart") {
        state.close_display(&profile_id, &vm_id).await;
    }
    perform_action(&agent, &vm_id, action).await?;
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Agent events WebSocket
// ---------------------------------------------------------------------------

async fn agent_events(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> std::result::Result<Response, ApiError> {
    let agent = state.get_agent(&id).await?;
    let vms = agent.refresh().await?;
    let mut events = agent.subscribe();
    Ok(ws.on_upgrade(move |socket| async move {
        run_agent_ws(socket, agent, vms, &mut events).await;
    }))
}

async fn run_agent_ws(
    mut socket: WebSocket,
    agent: Arc<Agent>,
    initial: Vec<VmInfo>,
    events: &mut tokio::sync::broadcast::Receiver<Arc<AgentEvent>>,
) {
    let hello = json!({
        "type": "hello",
        "model": agent.client.model,
        "connection_fingerprint": agent.client.connection_fingerprint_string(),
        "vms": initial,
    });
    if send_json(&mut socket, &hello).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Text(text))) => {
                        if text.contains("ping") {
                            let _ = send_json(&mut socket, &json!({"type":"pong"})).await;
                        }
                    }
                    Some(Ok(Message::Ping(data))) => {
                        let _ = socket.send(Message::Pong(data)).await;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
            event = events.recv() => {
                match event {
                    Ok(event) => {
                        let disconnected = matches!(*event, AgentEvent::Disconnected { .. });
                        if send_json(&mut socket, &*event).await.is_err() {
                            break;
                        }
                        if disconnected {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        if let Err(error) = agent.refresh().await {
                            debug!("resync refresh failed: {error}");
                        }
                        let vms = agent.vms();
                        if send_json(&mut socket, &json!({"type":"vms","vms":vms})).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

async fn send_json(socket: &mut WebSocket, value: &impl Serialize) -> std::result::Result<(), ()> {
    let text = serde_json::to_string(value).map_err(|_| ())?;
    socket
        .send(Message::Text(text.into()))
        .await
        .map_err(|_| ())
}

// ---------------------------------------------------------------------------
// Display WebSocket
// ---------------------------------------------------------------------------

struct ViewerGuard(Arc<DisplaySession>);

impl ViewerGuard {
    fn new(session: Arc<DisplaySession>) -> Self {
        session.viewers.fetch_add(1, Ordering::SeqCst);
        session.touch();
        Self(session)
    }
}

impl Drop for ViewerGuard {
    fn drop(&mut self) {
        self.0.viewers.fetch_sub(1, Ordering::SeqCst);
        self.0.touch();
    }
}

async fn display_ws(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    Path((profile_id, vm_id)): Path<(String, String)>,
) -> std::result::Result<Response, ApiError> {
    let session = state.ensure_display(&profile_id, &vm_id).await?;
    let events = session.subscribe();
    Ok(ws.on_upgrade(move |socket| async move {
        run_display_ws(socket, session, events).await;
    }))
}

async fn run_display_ws(
    mut socket: WebSocket,
    session: Arc<DisplaySession>,
    mut events: tokio::sync::broadcast::Receiver<Arc<DisplayEvent>>,
) {
    let _guard = ViewerGuard::new(Arc::clone(&session));
    if send_json(
        &mut socket,
        &json!({
            "type": "ready",
            "vm": session.vm_id,
            "name": session.vm_name,
            "width": session.width.load(Ordering::Relaxed),
            "height": session.height.load(Ordering::Relaxed),
        }),
    )
    .await
    .is_err()
    {
        return;
    }
    if let Some(frame) = session.full_frame() {
        if socket
            .send(Message::Binary(frame_message(&frame).into()))
            .await
            .is_err()
        {
            return;
        }
    }
    let mouse_mode = session.mouse_mode.load(Ordering::Relaxed);
    let _ = socket
        .send(Message::Binary(mouse_mode_message(mouse_mode).into()))
        .await;

    let mut buttons: u16 = 0;
    let mut last_position: Option<(u32, u32)> = None;
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Text(text))) => {
                        if handle_input(&session, &text, &mut buttons, &mut last_position, &mut socket).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Ping(data))) => {
                        let _ = socket.send(Message::Pong(data)).await;
                    }
                    Some(Ok(Message::Binary(_))) => {}
                    Some(Err(_)) => break,
                    Some(Ok(_)) => {}
                }
            }
            event = events.recv() => {
                match event {
                    Ok(event) => {
                        if send_display_event(&mut socket, &event).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        if let Some(frame) = session.full_frame() {
                            if socket.send(Message::Binary(frame_message(&frame).into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

async fn send_display_event(
    socket: &mut WebSocket,
    event: &DisplayEvent,
) -> std::result::Result<(), ()> {
    match event {
        DisplayEvent::Resize { width, height } => {
            let mut out = Vec::with_capacity(9);
            out.push(2u8);
            out.extend_from_slice(&width.to_le_bytes());
            out.extend_from_slice(&height.to_le_bytes());
            socket
                .send(Message::Binary(out.into()))
                .await
                .map_err(|_| ())
        }
        DisplayEvent::Frame(frame) => socket
            .send(Message::Binary(frame_message(frame).into()))
            .await
            .map_err(|_| ()),
        DisplayEvent::MouseMode(mode) => socket
            .send(Message::Binary(mouse_mode_message(*mode).into()))
            .await
            .map_err(|_| ()),
        DisplayEvent::Notice(message) => {
            let mut out = Vec::with_capacity(message.len() + 1);
            out.push(3u8);
            out.extend_from_slice(message.as_bytes());
            socket
                .send(Message::Binary(out.into()))
                .await
                .map_err(|_| ())
        }
        DisplayEvent::Error(message) => {
            let mut out = Vec::with_capacity(message.len() + 1);
            out.push(4u8);
            out.extend_from_slice(message.as_bytes());
            socket
                .send(Message::Binary(out.into()))
                .await
                .map_err(|_| ())
        }
        DisplayEvent::Closed => socket
            .send(Message::Binary(vec![6u8].into()))
            .await
            .map_err(|_| ()),
    }
}

fn frame_message(frame: &Frame) -> Vec<u8> {
    let mut out = Vec::with_capacity(29 + frame.pixels.len());
    out.push(1u8);
    for value in [
        frame.x,
        frame.y,
        frame.w,
        frame.h,
        frame.surface_w,
        frame.surface_h,
        frame.seq as u32,
    ] {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.extend_from_slice(&frame.pixels);
    out
}

fn mouse_mode_message(mode: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(3);
    out.push(5u8);
    out.extend_from_slice(&mode.to_le_bytes());
    out
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum InputMessage {
    Key { code: String, down: bool },
    Mouse { x: u32, y: u32 },
    Button { button: u8, down: bool },
    Wheel { delta: i32 },
    Modifiers { modifiers: u16 },
    Refresh,
    Ping,
}

async fn handle_input(
    session: &Arc<DisplaySession>,
    text: &str,
    buttons: &mut u16,
    last_position: &mut Option<(u32, u32)>,
    socket: &mut WebSocket,
) -> std::result::Result<(), ()> {
    let message: InputMessage = match serde_json::from_str(text) {
        Ok(message) => message,
        Err(error) => {
            debug!("invalid input message: {error}");
            return Ok(());
        }
    };
    let mode = session.mouse_mode.load(Ordering::Relaxed);
    match message {
        InputMessage::Key { code, down } => {
            let _ = session
                .input_tx
                .send(InputCommand::Key { code, down })
                .await;
        }
        InputMessage::Mouse { x, y } => {
            if mode == MOUSE_MODE_CLIENT {
                let _ = session
                    .input_tx
                    .send(InputCommand::MousePosition {
                        x,
                        y,
                        buttons: *buttons,
                        display_id: 0,
                    })
                    .await;
            } else if let Some((last_x, last_y)) = *last_position {
                let dx = x as i32 - last_x as i32;
                let dy = y as i32 - last_y as i32;
                if dx != 0 || dy != 0 {
                    let _ = session
                        .input_tx
                        .send(InputCommand::MouseMotion {
                            dx,
                            dy,
                            buttons: *buttons,
                        })
                        .await;
                }
            }
            *last_position = Some((x, y));
        }
        InputMessage::Button { button, down } => {
            let mask = mouse_button_mask(button);
            if down {
                *buttons |= mask;
                let _ = session
                    .input_tx
                    .send(InputCommand::MousePress {
                        button,
                        buttons: *buttons,
                    })
                    .await;
            } else {
                *buttons &= !mask;
                let _ = session
                    .input_tx
                    .send(InputCommand::MouseRelease {
                        button,
                        buttons: *buttons,
                    })
                    .await;
            }
        }
        InputMessage::Wheel { delta } => {
            let (button, mask) = if delta < 0 { (3u8, 8u16) } else { (4u8, 16u16) };
            let steps = delta.unsigned_abs().min(10);
            for _ in 0..steps {
                let pressed = *buttons | mask;
                let _ = session
                    .input_tx
                    .send(InputCommand::MousePress {
                        button,
                        buttons: pressed,
                    })
                    .await;
                let _ = session
                    .input_tx
                    .send(InputCommand::MouseRelease {
                        button,
                        buttons: *buttons,
                    })
                    .await;
            }
        }
        InputMessage::Modifiers { modifiers } => {
            let _ = session
                .input_tx
                .send(InputCommand::KeyModifiers { modifiers })
                .await;
        }
        InputMessage::Refresh => {
            if let Some(frame) = session.full_frame() {
                socket
                    .send(Message::Binary(frame_message(&frame).into()))
                    .await
                    .map_err(|_| ())?;
            }
        }
        InputMessage::Ping => {
            send_json(socket, &json!({ "type": "pong" })).await?;
        }
    }
    Ok(())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[allow(dead_code)]
fn _status_codes() {
    let _ = StatusCode::OK;
    let _ = HeaderValue::from_static("text/plain");
    let _ = StartOptions::NONE;
    let _ = StopMethod::Request;
    let _ = Duration::from_secs(0);
}
