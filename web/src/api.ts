// Typed HTTP client for the utm-web API.

export interface ProfileView {
  id: string;
  name: string;
  host: string;
  port: number;
  fingerprint: string | null;
  trusted: boolean;
  has_password: boolean;
  last_connected: number | null;
}

export type VmState =
  | "stopped"
  | "starting"
  | "started"
  | "pausing"
  | "paused"
  | "resuming"
  | "saving"
  | "restoring"
  | "stopping";

export interface VmInfo {
  id: string;
  name: string;
  path: string;
  is_shortcut: boolean;
  is_suspended: boolean;
  is_takeover_allowed: boolean;
  backend: string;
  state: VmState;
  mounted_drives: Record<string, string>;
}

export interface Status {
  version: string;
  client_fingerprint: string;
  allow_untrusted: boolean;
  profiles: number;
  max_sessions: number;
  session_idle_timeout_secs: number;
  public_url: string | null;
  uptime_secs: number;
}

export interface ConnectResponse {
  profile: ProfileView;
  model: string;
  connection_fingerprint: string;
  server_fingerprint: string;
  client_fingerprint: string;
  vms: VmInfo[];
}

export interface ProfileInput {
  name?: string;
  host?: string;
  port?: number;
  password?: string;
  fingerprint?: string;
  trusted?: boolean;
}

export interface DiscoveredServer {
  id: string;
  name: string;
  host: string;
  port: number;
  addresses: string[];
}

export interface DiscoverySnapshot {
  available: boolean;
  error: string | null;
  servers: DiscoveredServer[];
}

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
    readonly fingerprint?: string,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

async function request<T>(path: string, init: RequestInit = {}): Promise<T> {
  const headers = new Headers(init.headers);
  if (init.body !== undefined && !headers.has("content-type")) {
    headers.set("content-type", "application/json");
  }
  const response = await fetch(path, { ...init, headers });
  const text = await response.text();
  let body: unknown = null;
  if (text) {
    try {
      body = JSON.parse(text);
    } catch {
      body = text;
    }
  }
  if (!response.ok) {
    const record = (body ?? {}) as Record<string, unknown>;
    throw new ApiError(
      response.status,
      typeof record.error === "string" ? record.error : "http_error",
      typeof record.message === "string" ? record.message : response.statusText,
      typeof record.fingerprint === "string" ? record.fingerprint : undefined,
    );
  }
  return body as T;
}

function json(body: unknown): RequestInit {
  return { method: "POST", body: JSON.stringify(body) };
}

export const api = {
  status: () => request<Status>("/api/status"),
  discovery: () => request<DiscoverySnapshot>("/api/discovery"),
  profiles: () => request<ProfileView[]>("/api/profiles"),
  createProfile: (input: ProfileInput) =>
    request<ProfileView>("/api/profiles", json(input)),
  updateProfile: (id: string, input: ProfileInput) =>
    request<ProfileView>(`/api/profiles/${encodeURIComponent(id)}`, {
      method: "PUT",
      body: JSON.stringify(input),
    }),
  deleteProfile: (id: string) =>
    request<{ ok: boolean }>(`/api/profiles/${encodeURIComponent(id)}`, {
      method: "DELETE",
    }),
  connect: (id: string) =>
    request<ConnectResponse>(`/api/profiles/${encodeURIComponent(id)}/connect`, {
      method: "POST",
    }),
  disconnect: (id: string) =>
    request<{ ok: boolean }>(`/api/profiles/${encodeURIComponent(id)}/disconnect`, {
      method: "POST",
    }),
  vms: (id: string) =>
    request<VmInfo[]>(`/api/profiles/${encodeURIComponent(id)}/vms`),
  action: (
    profileId: string,
    vmId: string,
    action: string,
    extra: { method?: string; tablet?: boolean } = {},
  ) =>
    request<{ ok: boolean }>(
      `/api/profiles/${encodeURIComponent(profileId)}/vms/${encodeURIComponent(vmId)}/action`,
      json({ action, ...extra }),
    ),
  trust: (id: string, fingerprint: string) =>
    request<ProfileView>(`/api/profiles/${encodeURIComponent(id)}/trust`, json({ fingerprint })),
};

export function eventsUrl(profileId: string): string {
  const scheme = location.protocol === "https:" ? "wss:" : "ws:";
  return `${scheme}//${location.host}/api/profiles/${encodeURIComponent(profileId)}/events`;
}

export function discoveryUrl(): string {
  const scheme = location.protocol === "https:" ? "wss:" : "ws:";
  return `${scheme}//${location.host}/api/discovery/events`;
}

export function displayUrl(profileId: string, vmId: string): string {
  const scheme = location.protocol === "https:" ? "wss:" : "ws:";
  return `${scheme}//${location.host}/api/display/${encodeURIComponent(profileId)}/${encodeURIComponent(vmId)}/ws`;
}
