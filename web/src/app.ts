// utm-web frontend: profiles, VM lifecycle and the SPICE display client.

import {
  api,
  ApiError,
  displayUrl,
  eventsUrl,
  type ConnectResponse,
  type ProfileInput,
  type ProfileView,
  type Status,
  type VmInfo,
  type VmState,
} from "./api";
import { DisplayClient, MOUSE_MODE_CLIENT } from "./display";

const SERVER_PORT_DEFAULT = 21589;

interface AgentMessage {
  type: string;
  vms?: VmInfo[];
  vm?: VmInfo;
  model?: string;
  connection_fingerprint?: string;
  reason?: string;
  message?: string;
  id?: string;
}

interface Connection {
  profileId: string;
  state: "connecting" | "open" | "closed";
  vms: VmInfo[];
  model: string;
  connectionFingerprint: string;
  serverFingerprint: string;
  ws: WebSocket | null;
  intentional: boolean;
  upstreamClosed: boolean;
  reconnectTimer: number;
  attempts: number;
}

const state = {
  status: null as Status | null,
  profiles: [] as ProfileView[],
  selectedId: null as string | null,
  connections: new Map<string, Connection>(),
  pendingTrust: new Map<string, string>(),
  displayClient: null as DisplayClient | null,
  displayProfileId: null as string | null,
  displayVmId: null as string | null,
};

const $ = <T extends HTMLElement>(id: string): T => {
  const element = document.getElementById(id);
  if (!element) throw new Error(`missing element #${id}`);
  return element as T;
};

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  attrs: Record<string, string | boolean | undefined> = {},
  ...children: Array<Node | string | null | undefined>
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (value === undefined || value === false) continue;
    if (key === "class") node.className = String(value);
    else if (key === "text") node.textContent = String(value);
    else if (value === true) node.setAttribute(key, "");
    else node.setAttribute(key, String(value));
  }
  for (const child of children) {
    if (child === null || child === undefined) continue;
    node.append(typeof child === "string" ? document.createTextNode(child) : child);
  }
  return node;
}

// ---------------------------------------------------------------------------
// Toasts and errors
// ---------------------------------------------------------------------------

function toast(message: string, kind: "info" | "error" | "success" = "info"): void {
  const container = $("toasts");
  const item = el("div", { class: `toast toast-${kind}`, role: "status" }, message);
  container.append(item);
  window.setTimeout(() => {
    item.classList.add("toast-out");
    window.setTimeout(() => item.remove(), 250);
  }, kind === "error" ? 8000 : 4000);
}

function errorMessage(error: unknown): string {
  if (error instanceof ApiError) return error.message;
  if (error instanceof Error) return error.message;
  return String(error);
}

function handleProfileError(error: unknown, profileId: string): void {
  if (error instanceof ApiError && error.code === "untrusted" && error.fingerprint) {
    state.pendingTrust.set(profileId, error.fingerprint);
    const profile = state.profiles.find((item) => item.id === profileId);
    if (profile) profile.fingerprint = error.fingerprint;
    toast("Verify the server fingerprint before connecting.", "info");
  } else if (error instanceof ApiError && error.code === "fingerprint_mismatch") {
    toast(error.message, "error");
  } else {
    toast(errorMessage(error), "error");
  }
  render();
}

// ---------------------------------------------------------------------------
// Profiles
// ---------------------------------------------------------------------------

async function loadProfiles(): Promise<void> {
  try {
    state.profiles = await api.profiles();
    if (state.selectedId && !state.profiles.some((p) => p.id === state.selectedId)) {
      state.selectedId = null;
    }
    if (!state.selectedId && state.profiles.length > 0) {
      state.selectedId = state.profiles[0].id;
    }
    render();
  } catch (error) {
    toast(`Cannot load profiles: ${errorMessage(error)}`, "error");
  }
}

function replaceProfile(profile: ProfileView): void {
  const index = state.profiles.findIndex((item) => item.id === profile.id);
  if (index >= 0) state.profiles[index] = profile;
  else state.profiles.push(profile);
}

function renderProfileList(): void {
  const list = $("profileList");
  list.replaceChildren();
  for (const profile of state.profiles) {
    const connection = state.connections.get(profile.id);
    const online = connection?.state === "open";
    const connecting = connection?.state === "connecting";
    const item = el(
      "li",
      {
        class: `profile-item${profile.id === state.selectedId ? " selected" : ""}`,
        "data-id": profile.id,
        tabindex: "0",
        role: "button",
        "aria-current": profile.id === state.selectedId ? "true" : undefined,
      },
      el("span", {
        class: `status-dot${online ? " online" : connecting ? " pending" : ""}`,
        title: online ? "Connected" : connecting ? "Connecting" : "Not connected",
      }),
      el(
        "span",
        { class: "profile-item-text" },
        el("span", { class: "profile-item-name", text: profile.name }),
        el("span", { class: "profile-item-host", text: `${profile.host}:${profile.port}` }),
      ),
    );
    list.append(item);
  }
  $("profileEmpty").hidden = state.profiles.length > 0;
}

function renderStatus(): void {
  const status = state.status;
  if (!status) return;
  $("version").textContent = `v${status.version}`;
  $("clientFingerprint").textContent = status.client_fingerprint;
  $("clientFingerprint").title = `This client's SHA-256 fingerprint: ${status.client_fingerprint}`;
  $("untrustedBadge").hidden = !status.allow_untrusted;
}

function selectedProfile(): ProfileView | null {
  return state.profiles.find((profile) => profile.id === state.selectedId) ?? null;
}

function renderProfile(): void {
  const profile = selectedProfile();
  const pane = $("profilePane");
  const empty = $("emptyState");
  if (!profile) {
    pane.hidden = true;
    empty.hidden = state.profiles.length > 0 ? false : true;
    if (state.profiles.length === 0) {
      empty.hidden = false;
      $("emptyTitle").textContent = "No servers yet";
      $("emptyText").textContent =
        "Add your Mac running UTM Remote to get started. UTM must have Remote access enabled in Settings.";
    } else {
      empty.hidden = false;
      $("emptyTitle").textContent = "Select a server";
      $("emptyText").textContent = "Choose a server in the sidebar to see its virtual machines.";
    }
    return;
  }
  empty.hidden = true;
  pane.hidden = false;
  $("profileName").textContent = profile.name;
  $("profileEndpoint").textContent = `${profile.host}:${profile.port}`;
  $("profileMeta").textContent = profile.trusted
    ? "Certificate trusted"
    : profile.fingerprint
      ? "Certificate not trusted"
      : "Certificate not verified";

  const connection = state.connections.get(profile.id);
  const connected = connection?.state === "open";
  const connecting = connection?.state === "connecting";
  $("connectBtn").hidden = connected || connecting;
  $("disconnectBtn").hidden = !connected && !connecting;
  $("connectBtn").textContent = connecting ? "Connecting…" : "Connect";
  if (connecting) $("connectBtn").setAttribute("disabled", "");
  else $("connectBtn").removeAttribute("disabled");

  const fingerprints = $("fingerprints");
  fingerprints.replaceChildren();
  const addFingerprint = (label: string, value: string | null | undefined) => {
    if (!value) return;
    fingerprints.append(
      el(
        "div",
        { class: "fingerprint-row" },
        el("span", { class: "fingerprint-label", text: label }),
        el("code", { class: "fingerprint-value", text: value, title: value }),
      ),
    );
  };
  addFingerprint("Server", profile.fingerprint);
  addFingerprint("Connection", connection?.connectionFingerprint);
  addFingerprint("Client", state.status?.client_fingerprint);
  if (connection?.model) addFingerprint("Model", connection.model);

  const pending = state.pendingTrust.get(profile.id);
  const banner = $("trustBanner");
  if (pending || (profile.fingerprint && !profile.trusted)) {
    banner.hidden = false;
    $("trustFingerprint").textContent = pending ?? profile.fingerprint ?? "";
    $("trustHint").textContent = pending
      ? "The first connection was refused. Compare this fingerprint with the one shown in UTM on your Mac."
      : "This server is not trusted. Compare the fingerprint with the one shown in UTM on your Mac.";
  } else {
    banner.hidden = true;
  }

  renderVms(connection);
}

function stateLabel(vmState: VmState): string {
  return vmState.charAt(0).toUpperCase() + vmState.slice(1);
}

function isBusy(vmState: VmState): boolean {
  return ["starting", "pausing", "resuming", "saving", "restoring", "stopping"].includes(vmState);
}

interface VmButton {
  label: string;
  action: string;
  method?: string;
  primary?: boolean;
  danger?: boolean;
  confirm?: string;
}

function vmButtons(vm: VmInfo): VmButton[] {
  if (isBusy(vm.state)) return [];
  switch (vm.state) {
    case "stopped":
      return [{ label: "Start", action: "start", primary: true }];
    case "paused":
      return [
        { label: "Display", action: "display", primary: true },
        { label: "Resume", action: "resume" },
        { label: "Stop", action: "stop", confirm: `Stop ${vm.name}?` },
        { label: "Force stop", action: "stop", method: "force", danger: true, confirm: `Force stop ${vm.name}? Unsaved data may be lost.` },
      ];
    case "started":
      return [
        { label: "Display", action: "display", primary: true },
        { label: "Pause", action: "pause" },
        { label: "Restart", action: "restart", confirm: `Restart ${vm.name}?` },
        { label: "Stop", action: "stop", confirm: `Stop ${vm.name}?` },
        { label: "Force stop", action: "stop", method: "force", danger: true, confirm: `Force stop ${vm.name}? Unsaved data may be lost.` },
        { label: "Guest tools", action: "mount-guest-tools" },
      ];
    default:
      return [];
  }
}

function renderVms(connection: Connection | undefined): void {
  const list = $("vmList");
  list.replaceChildren();
  const connected = connection?.state === "open";
  $("vmToolbar").hidden = !connected;
  $("vmHint").textContent = connected
    ? `${connection?.vms.length ?? 0} virtual machine${(connection?.vms.length ?? 0) === 1 ? "" : "s"}`
    : connection?.state === "connecting"
      ? "Connecting to the UTM server…"
      : "Connect to this server to list its virtual machines.";

  if (!connected) {
    $("vmToolbar").hidden = true;
    return;
  }

  const vms = connection?.vms ?? [];
  if (vms.length === 0) {
    list.append(el("p", { class: "muted pad", text: "No virtual machines found on this server." }));
    return;
  }

  for (const vm of vms) {
    const busy = isBusy(vm.state);
    const card = el("article", { class: `vm-card state-${vm.state}`, "data-vm": vm.id });
    const head = el(
      "div",
      { class: "vm-head" },
      el("div", { class: "vm-title" },
        el("h3", { text: vm.name || vm.id }),
        el("span", { class: `badge badge-${vm.state}`, text: stateLabel(vm.state) }),
      ),
      el("div", { class: "vm-id", text: vm.id }),
    );
    const metaParts = [vm.backend, vm.path].filter(Boolean);
    if (vm.is_suspended) metaParts.push("suspended");
    const meta = el("p", { class: "vm-meta", text: metaParts.join(" · ") });
    const actions = el("div", { class: "vm-actions" });
    for (const button of vmButtons(vm)) {
      const attrs: Record<string, string | boolean | undefined> = {
        class: `btn small${button.primary ? " primary" : ""}${button.danger ? " danger" : ""}`,
        "data-action": button.action,
        "data-vm": vm.id,
      };
      if (button.method) attrs["data-method"] = button.method;
      if (button.confirm) attrs["data-confirm"] = button.confirm;
      actions.append(el("button", attrs, button.label));
    }
    if (busy) {
      actions.append(el("span", { class: "muted small", text: `${stateLabel(vm.state)}…` }));
    }
    card.append(head, meta);
    const drives = Object.entries(vm.mounted_drives ?? {});
    if (drives.length > 0) {
      card.append(
        el("p", {
          class: "vm-meta",
          text: `Drives: ${drives.map(([key, value]) => `${key}=${value}`).join(", ")}`,
        }),
      );
    }
    card.append(actions);
    list.append(card);
  }
}

function render(): void {
  renderStatus();
  renderProfileList();
  renderProfile();
}

// ---------------------------------------------------------------------------
// Agent event stream
// ---------------------------------------------------------------------------

function connectionFor(profileId: string): Connection {
  let connection = state.connections.get(profileId);
  if (!connection) {
    connection = {
      profileId,
      state: "connecting",
      vms: [],
      model: "",
      connectionFingerprint: "",
      serverFingerprint: "",
      ws: null,
      intentional: false,
      upstreamClosed: false,
      reconnectTimer: 0,
      attempts: 0,
    };
    state.connections.set(profileId, connection);
  }
  return connection;
}

async function connectProfile(profileId: string): Promise<boolean> {
  const existing = state.connections.get(profileId);
  if (existing && (existing.state === "open" || existing.state === "connecting")) {
    return existing.state === "open";
  }
  const connection = connectionFor(profileId);
  connection.state = "connecting";
  connection.intentional = false;
  connection.upstreamClosed = false;
  render();
  try {
    const response = await api.connect(profileId);
    applyConnectResponse(connection, response);
    openEvents(connection);
    render();
    return true;
  } catch (error) {
    state.connections.delete(profileId);
    handleProfileError(error, profileId);
    return false;
  }
}

function applyConnectResponse(connection: Connection, response: ConnectResponse): void {
  connection.vms = response.vms;
  connection.model = response.model;
  connection.connectionFingerprint = response.connection_fingerprint;
  connection.serverFingerprint = response.server_fingerprint;
  replaceProfile(response.profile);
}

function openEvents(connection: Connection): void {
  connection.ws?.close();
  const socket = new WebSocket(eventsUrl(connection.profileId));
  connection.ws = socket;
  connection.state = "connecting";

  socket.addEventListener("open", () => {
    connection.attempts = 0;
  });

  socket.addEventListener("message", (event) => {
    if (typeof event.data !== "string") return;
    let message: AgentMessage;
    try {
      message = JSON.parse(event.data);
    } catch {
      return;
    }
    switch (message.type) {
      case "hello":
        connection.state = "open";
        connection.vms = message.vms ?? [];
        connection.model = message.model ?? connection.model;
        connection.connectionFingerprint =
          message.connection_fingerprint ?? connection.connectionFingerprint;
        render();
        break;
      case "vms":
        connection.vms = message.vms ?? [];
        render();
        break;
      case "vm": {
        const updated = message.vm;
        if (!updated) break;
        const index = connection.vms.findIndex((vm) => vm.id === updated.id);
        if (index >= 0) connection.vms[index] = updated;
        else connection.vms.push(updated);
        render();
        break;
      }
      case "error":
        toast(`VM error: ${message.message ?? "unknown error"}`, "error");
        break;
      case "disconnected":
        connection.upstreamClosed = true;
        connection.state = "closed";
        connection.intentional = true;
        toast(`Disconnected: ${message.reason ?? "connection closed"}`, "error");
        render();
        break;
    }
  });

  socket.addEventListener("close", () => {
    if (connection.ws !== socket) return;
    connection.ws = null;
    if (connection.intentional || connection.upstreamClosed) {
      connection.state = "closed";
      render();
      return;
    }
    connection.state = "closed";
    render();
    scheduleAgentReconnect(connection);
  });

  socket.addEventListener("error", () => {
    // close follows
  });
}

function scheduleAgentReconnect(connection: Connection): void {
  if (connection.intentional || connection.upstreamClosed) return;
  connection.attempts += 1;
  if (connection.attempts > 6) {
    toast("Lost the event stream. Click Connect to try again.", "error");
    return;
  }
  const delay = Math.min(1000 * connection.attempts, 8000);
  window.clearTimeout(connection.reconnectTimer);
  connection.reconnectTimer = window.setTimeout(async () => {
    const current = state.connections.get(connection.profileId);
    if (!current || current.intentional || current.upstreamClosed) return;
    try {
      const response = await api.connect(connection.profileId);
      applyConnectResponse(current, response);
      openEvents(current);
      render();
    } catch (error) {
      current.attempts += 1;
      scheduleAgentReconnect(current);
      if (current.attempts > 6) handleProfileError(error, connection.profileId);
    }
  }, delay);
}

async function disconnectProfile(profileId: string): Promise<void> {
  const connection = state.connections.get(profileId);
  if (connection) {
    connection.intentional = true;
    window.clearTimeout(connection.reconnectTimer);
    connection.ws?.close();
    connection.ws = null;
    state.connections.delete(profileId);
  }
  try {
    await api.disconnect(profileId);
    toast("Disconnected.");
  } catch (error) {
    toast(errorMessage(error), "error");
  }
  render();
}

async function refreshVms(profileId: string): Promise<void> {
  const connection = state.connections.get(profileId);
  if (!connection || connection.state !== "open") return;
  try {
    connection.vms = await api.vms(profileId);
    render();
  } catch (error) {
    handleProfileError(error, profileId);
  }
}

// ---------------------------------------------------------------------------
// VM actions
// ---------------------------------------------------------------------------

async function performVmAction(
  profileId: string,
  vmId: string,
  action: string,
  method?: string,
  confirmText?: string,
): Promise<void> {
  if (confirmText && !window.confirm(confirmText)) return;
  try {
    await api.action(profileId, vmId, action, method ? { method } : {});
    const labels: Record<string, string> = {
      start: "Starting…",
      stop: method === "force" ? "Force stopping…" : "Stopping…",
      restart: "Restarting…",
      pause: "Pausing…",
      resume: "Resuming…",
      "mount-guest-tools": "Mounting guest tools…",
    };
    toast(labels[action] ?? "Request sent.");
    if (action === "start" || action === "stop" || action === "restart" || action === "pause" || action === "resume") {
      window.setTimeout(() => void refreshVms(profileId), 1200);
    }
  } catch (error) {
    toast(errorMessage(error), "error");
  }
}

// ---------------------------------------------------------------------------
// Display
// ---------------------------------------------------------------------------

function displayOverlay(visible: boolean, text = "", detail = ""): void {
  const overlay = $("displayOverlay");
  overlay.hidden = !visible;
  $("displayOverlayText").textContent = text;
  $("displayOverlayDetail").textContent = detail;
}

function setDisplayStatus(text: string): void {
  $("displayStatus").textContent = text;
}

function updateMouseMode(mode: number): void {
  const label = mode === MOUSE_MODE_CLIENT ? "Absolute pointer" : mode === 1 ? "Relative pointer" : "Pointer mode unknown";
  $("displayMouseMode").textContent = label;
}

async function openDisplay(profile: ProfileView, vm: VmInfo): Promise<void> {
  const connection = state.connections.get(profile.id);
  if (!connection || connection.state !== "open") {
    const connected = await connectProfile(profile.id);
    if (!connected) return;
  }
  if (state.displayClient) {
    state.displayClient.close();
    state.displayClient = null;
  }
  state.displayProfileId = profile.id;
  state.displayVmId = vm.id;
  $("displayTitle").textContent = vm.name || vm.id;
  $("displaySubtitle").textContent = `${profile.name} · ${profile.host}`;
  setDisplayStatus("Connecting…");
  updateMouseMode(0);
  displayOverlay(true, "Connecting to the display…", "SPICE is negotiating the session.");
  const canvas = $<HTMLCanvasElement>("displayCanvas");
  canvas.width = 0;
  canvas.height = 0;
  canvas.style.aspectRatio = "";
  const dialog = $<HTMLDialogElement>("displayDialog");
  if (!dialog.open) dialog.showModal();

  const client = new DisplayClient(canvas, displayUrl(profile.id, vm.id), {
    onStatus(status, detail) {
      switch (status) {
        case "connecting":
          setDisplayStatus("Connecting…");
          displayOverlay(true, "Connecting to the display…", detail ?? "");
          break;
        case "live":
          setDisplayStatus("Live");
          displayOverlay(false);
          break;
        case "reconnecting":
          setDisplayStatus("Reconnecting…");
          displayOverlay(true, "Reconnecting…", detail ?? "");
          break;
        case "closed":
          setDisplayStatus("Closed");
          displayOverlay(true, detail ?? "The display session ended.", "Use Reconnect to open it again.");
          break;
      }
    },
    onNotice(message) {
      toast(message);
    },
    onError(message) {
      toast(message, "error");
      displayOverlay(true, "Display error", message);
    },
    onMouseMode(mode) {
      updateMouseMode(mode);
    },
  });
  state.displayClient = client;
  client.attachInput();
  client.start();
  canvas.focus();
}

function closeDisplay(): void {
  const client = state.displayClient;
  state.displayClient = null;
  state.displayProfileId = null;
  state.displayVmId = null;
  client?.close();
  const dialog = $<HTMLDialogElement>("displayDialog");
  if (dialog.open) dialog.close();
  displayOverlay(false);
}

async function toggleFullscreen(): Promise<void> {
  const shell = $("displayShell");
  try {
    if (document.fullscreenElement) {
      await document.exitFullscreen();
    } else if (shell.requestFullscreen) {
      await shell.requestFullscreen();
    } else {
      shell.classList.toggle("maximized");
    }
  } catch (error) {
    shell.classList.toggle("maximized");
    toast(errorMessage(error), "error");
  }
}

function updateFullscreenButton(): void {
  const active = Boolean(document.fullscreenElement);
  $("displayFullscreen").textContent = active ? "Exit fullscreen" : "Fullscreen";
  $("displayShell").classList.toggle("is-fullscreen", active);
}

// ---------------------------------------------------------------------------
// Profile dialog
// ---------------------------------------------------------------------------

function openProfileDialog(profile: ProfileView | null): void {
  const dialog = $<HTMLDialogElement>("profileDialog");
  const form = $<HTMLFormElement>("profileForm");
  form.reset();
  $("profileDialogTitle").textContent = profile ? "Edit server" : "Add server";
  $<HTMLInputElement>("profileId").value = profile?.id ?? "";
  $<HTMLInputElement>("profileNameInput").value = profile?.name ?? "";
  $<HTMLInputElement>("profileHostInput").value = profile?.host ?? "";
  $<HTMLInputElement>("profilePortInput").value = String(profile?.port ?? SERVER_PORT_DEFAULT);
  $<HTMLInputElement>("profilePasswordInput").value = "";
  $<HTMLInputElement>("profilePasswordInput").placeholder = profile?.has_password ? "Leave blank to keep the saved password" : "Optional";
  $<HTMLInputElement>("profileFingerprintInput").value = profile?.fingerprint ?? "";
  $<HTMLInputElement>("profileTrustedInput").checked = Boolean(profile?.trusted);
  $<HTMLInputElement>("profileTrustedInput").disabled = !profile?.fingerprint;
  $("profileTrustedHint").textContent = profile?.fingerprint
    ? "Only enable this after verifying the fingerprint."
    : "A fingerprint is required to pre-trust a server; otherwise you will be asked on first connection.";
  dialog.showModal();
  $<HTMLInputElement>("profileNameInput").focus();
}

function collectProfileInput(editing: boolean): ProfileInput {
  const input: ProfileInput = {
    name: $<HTMLInputElement>("profileNameInput").value.trim(),
    host: $<HTMLInputElement>("profileHostInput").value.trim(),
    port: Number($<HTMLInputElement>("profilePortInput").value),
  };
  const password = $<HTMLInputElement>("profilePasswordInput").value;
  if (password !== "" || !editing) input.password = password;
  const fingerprint = $<HTMLInputElement>("profileFingerprintInput").value.trim();
  if (fingerprint !== "") input.fingerprint = fingerprint;
  else if (editing) input.fingerprint = "";
  const trusted = $<HTMLInputElement>("profileTrustedInput");
  input.trusted = trusted.checked && Boolean(input.fingerprint);
  return input;
}

async function saveProfile(event: SubmitEvent): Promise<void> {
  event.preventDefault();
  const id = $<HTMLInputElement>("profileId").value;
  const input = collectProfileInput(Boolean(id));
  if (!input.host) {
    toast("Host is required.", "error");
    return;
  }
  if (!input.port || input.port < 1 || input.port > 65535) {
    toast("Port must be between 1 and 65535.", "error");
    return;
  }
  try {
    const profile = id ? await api.updateProfile(id, input) : await api.createProfile(input);
    replaceProfile(profile);
    state.selectedId = profile.id;
    state.pendingTrust.delete(profile.id);
    $<HTMLDialogElement>("profileDialog").close();
    toast(id ? "Server updated." : "Server added.", "success");
    render();
    if (!id) await connectProfile(profile.id);
  } catch (error) {
    toast(errorMessage(error), "error");
  }
}

async function deleteProfile(profile: ProfileView): Promise<void> {
  if (!window.confirm(`Delete the server "${profile.name}"? This only removes the saved profile.`)) return;
  try {
    await disconnectProfile(profile.id);
    await api.deleteProfile(profile.id);
    state.profiles = state.profiles.filter((item) => item.id !== profile.id);
    if (state.selectedId === profile.id) {
      state.selectedId = state.profiles[0]?.id ?? null;
    }
    toast("Server deleted.");
    render();
  } catch (error) {
    toast(errorMessage(error), "error");
  }
}

async function trustProfile(profileId: string): Promise<void> {
  const fingerprint = state.pendingTrust.get(profileId) ??
    state.profiles.find((profile) => profile.id === profileId)?.fingerprint;
  if (!fingerprint) return;
  try {
    const profile = await api.trust(profileId, fingerprint);
    replaceProfile(profile);
    state.pendingTrust.delete(profileId);
    toast("Server trusted.", "success");
    render();
    await connectProfile(profileId);
  } catch (error) {
    toast(errorMessage(error), "error");
  }
}

// ---------------------------------------------------------------------------
// Event wiring
// ---------------------------------------------------------------------------

function wireEvents(): void {
  $("addProfileBtn").addEventListener("click", () => openProfileDialog(null));
  $("emptyAddBtn").addEventListener("click", () => openProfileDialog(null));
  $("refreshProfilesBtn").addEventListener("click", () => void loadProfiles());

  const list = $("profileList");
  list.addEventListener("click", (event) => {
    const item = (event.target as HTMLElement).closest<HTMLElement>(".profile-item");
    if (!item) return;
    state.selectedId = item.dataset.id ?? null;
    render();
  });
  list.addEventListener("keydown", (event) => {
    if (event.key !== "Enter" && event.key !== " ") return;
    const item = (event.target as HTMLElement).closest<HTMLElement>(".profile-item");
    if (!item) return;
    event.preventDefault();
    state.selectedId = item.dataset.id ?? null;
    render();
  });

  $("connectBtn").addEventListener("click", () => {
    if (state.selectedId) void connectProfile(state.selectedId);
  });
  $("disconnectBtn").addEventListener("click", () => {
    if (state.selectedId) void disconnectProfile(state.selectedId);
  });
  $("editProfileBtn").addEventListener("click", () => {
    const profile = selectedProfile();
    if (profile) openProfileDialog(profile);
  });
  $("deleteProfileBtn").addEventListener("click", () => {
    const profile = selectedProfile();
    if (profile) void deleteProfile(profile);
  });
  $("trustBtn").addEventListener("click", () => {
    if (state.selectedId) void trustProfile(state.selectedId);
  });
  $("refreshVmsBtn").addEventListener("click", () => {
    if (state.selectedId) void refreshVms(state.selectedId);
  });

  $("vmList").addEventListener("click", (event) => {
    const button = (event.target as HTMLElement).closest<HTMLButtonElement>("button[data-action]");
    if (!button || !state.selectedId) return;
    const vmId = button.dataset.vm;
    if (!vmId) return;
    const action = button.dataset.action ?? "";
    if (action === "display") {
      const profile = selectedProfile();
      const connection = state.connections.get(state.selectedId);
      const vm = connection?.vms.find((item) => item.id === vmId);
      if (profile && vm) void openDisplay(profile, vm);
      return;
    }
    void performVmAction(
      state.selectedId,
      vmId,
      action,
      button.dataset.method,
      button.dataset.confirm,
    );
  });

  $<HTMLFormElement>("profileForm").addEventListener("submit", (event) => {
    void saveProfile(event);
  });
  $("profileCancel").addEventListener("click", () => {
    $<HTMLDialogElement>("profileDialog").close();
  });
  $<HTMLInputElement>("profileFingerprintInput").addEventListener("input", () => {
    const fingerprint = $<HTMLInputElement>("profileFingerprintInput").value.trim();
    const trusted = $<HTMLInputElement>("profileTrustedInput");
    trusted.disabled = fingerprint === "";
    if (fingerprint === "") trusted.checked = false;
  });

  $("displayClose").addEventListener("click", closeDisplay);
  $("displayReconnect").addEventListener("click", () => {
    displayOverlay(true, "Reconnecting…");
    state.displayClient?.reconnectNow();
  });
  $("displayRefresh").addEventListener("click", () => state.displayClient?.refresh());
  $("displayFullscreen").addEventListener("click", () => void toggleFullscreen());
  $("displayCad").addEventListener("click", () => state.displayClient?.sendCtrlAltDel());
  $<HTMLDialogElement>("displayDialog").addEventListener("close", () => {
    if (state.displayClient) {
      const client = state.displayClient;
      state.displayClient = null;
      client.close();
    }
    state.displayProfileId = null;
    state.displayVmId = null;
    displayOverlay(false);
  });
  $<HTMLDialogElement>("displayDialog").addEventListener("cancel", (event) => {
    // Esc should not drop the session by accident; the Close button is explicit.
    event.preventDefault();
  });
  document.addEventListener("fullscreenchange", updateFullscreenButton);

  window.addEventListener("beforeunload", () => {
    state.displayClient?.close();
  });
}

async function init(): Promise<void> {
  wireEvents();
  try {
    state.status = await api.status();
    renderStatus();
  } catch (error) {
    toast(`Cannot reach the utm-web backend: ${errorMessage(error)}`, "error");
  }
  await loadProfiles();
  render();
}

void init();
