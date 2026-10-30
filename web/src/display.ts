// SPICE display WebSocket client: binary frame decoding, canvas rendering and
// keyboard/mouse forwarding.

import { modifierMask, isMapped } from "./keyboard";

export const MOUSE_MODE_SERVER = 1;
export const MOUSE_MODE_CLIENT = 2;

export interface DisplayHooks {
  onStatus(state: "connecting" | "live" | "reconnecting" | "closed", detail?: string): void;
  onNotice(message: string): void;
  onError(message: string): void;
  onMouseMode(mode: number): void;
}

interface ReadyMessage {
  type: "ready";
  vm: string;
  name: string;
  width: number;
  height: number;
}

const FRAME_HEADER = 29;
const BUTTON_LEFT = 0;
const BUTTON_MIDDLE = 1;
const BUTTON_RIGHT = 2;

function clamp(value: number, min: number, max: number): number {
  return value < min ? min : value > max ? max : value;
}

function decodeText(bytes: Uint8Array): string {
  return new TextDecoder().decode(bytes);
}

export class DisplayClient {
  private ws: WebSocket | null = null;
  private readonly ctx: CanvasRenderingContext2D;
  private surfaceW = 0;
  private surfaceH = 0;
  private mouseMode = 0;
  private buttons = 0;
  private virtualX = 0;
  private virtualY = 0;
  private wheelAccum = 0;
  private closedByUser = false;
  private fatal = false;
  private reconnectTimer = 0;
  private reconnectAttempts = 0;
  private readonly maxReconnectAttempts = 6;
  private lastSeq: number | null = null;
  private readonly pressedModifiers = new Set<string>();
  private readonly pressedKeys = new Set<string>();
  private readonly cleanups: Array<() => void> = [];

  constructor(
    private readonly canvas: HTMLCanvasElement,
    private readonly url: string,
    private readonly hooks: DisplayHooks,
  ) {
    const ctx = canvas.getContext("2d", { alpha: false, desynchronized: true });
    if (!ctx) throw new Error("This browser does not support a 2D canvas");
    this.ctx = ctx;
    this.ctx.imageSmoothingEnabled = false;
  }

  start(): void {
    this.closedByUser = false;
    this.fatal = false;
    this.reconnectAttempts = 0;
    this.connect();
  }

  private connect(): void {
    this.hooks.onStatus(this.reconnectAttempts > 0 ? "reconnecting" : "connecting");
    let socket: WebSocket;
    try {
      socket = new WebSocket(this.url);
    } catch {
      this.scheduleReconnect();
      return;
    }
    socket.binaryType = "arraybuffer";
    this.ws = socket;

    socket.addEventListener("open", () => {
      this.lastSeq = null;
      this.hooks.onStatus("connecting", "waiting for the first frame");
    });

    socket.addEventListener("message", (event) => {
      if (typeof event.data === "string") {
        this.onText(event.data);
      } else if (event.data instanceof ArrayBuffer) {
        this.onBinary(event.data);
      }
    });

    socket.addEventListener("error", () => {
      // The close event carries the useful state.
    });

    socket.addEventListener("close", () => {
      if (this.ws === socket) this.ws = null;
      if (this.closedByUser) {
        this.hooks.onStatus("closed");
        return;
      }
      if (this.fatal) {
        this.hooks.onStatus("closed", "session ended");
        return;
      }
      this.scheduleReconnect();
    });
  }

  private scheduleReconnect(): void {
    if (this.closedByUser) return;
    this.reconnectAttempts += 1;
    if (this.reconnectAttempts > this.maxReconnectAttempts) {
      this.fatal = true;
      this.hooks.onStatus("closed", "Could not reconnect to the display session.");
      return;
    }
    const delay = Math.min(500 * this.reconnectAttempts, 5000);
    this.hooks.onStatus(
      "reconnecting",
      `connection lost; retrying in ${(delay / 1000).toFixed(1)}s`,
    );
    window.clearTimeout(this.reconnectTimer);
    this.reconnectTimer = window.setTimeout(() => this.connect(), delay);
  }

  reconnectNow(): void {
    window.clearTimeout(this.reconnectTimer);
    this.reconnectAttempts = 0;
    this.fatal = false;
    this.ws?.close();
    this.ws = null;
    this.connect();
  }

  close(): void {
    this.closedByUser = true;
    window.clearTimeout(this.reconnectTimer);
    this.detach();
    const socket = this.ws;
    this.ws = null;
    socket?.close();
    this.hooks.onStatus("closed");
  }

  private onText(text: string): void {
    let message: ReadyMessage | { type: string; [key: string]: unknown };
    try {
      message = JSON.parse(text);
    } catch {
      return;
    }
    if (message.type === "ready") {
      const ready = message as ReadyMessage;
      if (ready.width > 0 && ready.height > 0) {
        this.resize(ready.width, ready.height);
      }
      // Only a completed handshake proves the session is healthy.  Resetting
      // the retry budget on `open` would loop forever against a server that
      // accepts the socket and closes it again.
      this.reconnectAttempts = 0;
      this.lastSeq = null;
      this.hooks.onStatus("live");
    } else if (message.type === "pong") {
      // keep-alive acknowledgement
    }
  }

  private onBinary(data: ArrayBuffer): void {
    if (data.byteLength < 1) return;
    const view = new DataView(data);
    const kind = view.getUint8(0);
    switch (kind) {
      case 1: {
        if (data.byteLength < FRAME_HEADER) return;
        const x = view.getUint32(1, true);
        const y = view.getUint32(5, true);
        const w = view.getUint32(9, true);
        const h = view.getUint32(13, true);
        const surfaceW = view.getUint32(17, true);
        const surfaceH = view.getUint32(21, true);
        const seq = view.getUint32(25, true);
        if (w === 0 || h === 0) return;
        if (surfaceW !== this.surfaceW || surfaceH !== this.surfaceH) {
          this.resize(surfaceW, surfaceH);
        }
        // Sequence 0 is a full-surface snapshot (connect, lag recovery or an
        // explicit refresh).  Any other jump means we missed a dirty rect, so
        // ask for a fresh snapshot.  A snapshot resets the baseline because
        // the server's next partial frame continues from its own counter.
        if (seq === 0) {
          this.lastSeq = null;
        } else {
          if (this.lastSeq !== null && seq !== ((this.lastSeq + 1) >>> 0)) {
            this.refresh();
          }
          this.lastSeq = seq;
        }
        const length = w * h * 4;
        if (data.byteLength < FRAME_HEADER + length) return;
        if (x + w > this.surfaceW || y + h > this.surfaceH) return;
        const pixels = new Uint8ClampedArray(data, FRAME_HEADER, length);
        try {
          this.ctx.putImageData(new ImageData(pixels, w, h), x, y);
        } catch {
          // A malformed frame should not kill the session.
        }
        break;
      }
      case 2: {
        if (data.byteLength < 9) return;
        this.resize(view.getUint32(1, true), view.getUint32(5, true));
        break;
      }
      case 3:
        this.hooks.onNotice(decodeText(new Uint8Array(data, 1)));
        break;
      case 4:
        this.fatal = true;
        this.hooks.onError(decodeText(new Uint8Array(data, 1)));
        break;
      case 5: {
        if (data.byteLength < 3) return;
        this.mouseMode = view.getUint16(1, true);
        this.hooks.onMouseMode(this.mouseMode);
        break;
      }
      case 6:
        this.fatal = true;
        this.hooks.onStatus("closed", "the display session ended");
        break;
    }
  }

  private resize(width: number, height: number): void {
    if (width === 0 || height === 0) return;
    this.surfaceW = width;
    this.surfaceH = height;
    if (this.canvas.width !== width || this.canvas.height !== height) {
      this.canvas.width = width;
      this.canvas.height = height;
      this.ctx.imageSmoothingEnabled = false;
      this.ctx.fillStyle = "#000";
      this.ctx.fillRect(0, 0, width, height);
    }
    this.canvas.style.aspectRatio = `${width} / ${height}`;
    if (this.virtualX === 0 && this.virtualY === 0) {
      this.virtualX = Math.floor(width / 2);
      this.virtualY = Math.floor(height / 2);
    }
  }

  // -------------------------------------------------------------------------
  // Input
  // -------------------------------------------------------------------------

  attachInput(): void {
    const onPointerDown = (event: PointerEvent) => this.onPointerDown(event);
    const onPointerUp = (event: PointerEvent) => this.onPointerUp(event);
    const onPointerMove = (event: PointerEvent) => this.onPointerMove(event);
    const onPointerCancel = (event: PointerEvent) => this.onPointerUp(event);
    const onWheel = (event: WheelEvent) => this.onWheel(event);
    const onContextMenu = (event: Event) => event.preventDefault();
    const onKeyDown = (event: KeyboardEvent) => this.onKeyDown(event);
    const onKeyUp = (event: KeyboardEvent) => this.onKeyUp(event);
    const onBlur = () => this.releaseAll();

    this.canvas.addEventListener("pointerdown", onPointerDown);
    this.canvas.addEventListener("pointerup", onPointerUp);
    this.canvas.addEventListener("pointermove", onPointerMove);
    this.canvas.addEventListener("pointercancel", onPointerCancel);
    this.canvas.addEventListener("wheel", onWheel, { passive: false });
    this.canvas.addEventListener("contextmenu", onContextMenu);
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("keyup", onKeyUp);
    window.addEventListener("blur", onBlur);

    this.cleanups.push(() => {
      this.canvas.removeEventListener("pointerdown", onPointerDown);
      this.canvas.removeEventListener("pointerup", onPointerUp);
      this.canvas.removeEventListener("pointermove", onPointerMove);
      this.canvas.removeEventListener("pointercancel", onPointerCancel);
      this.canvas.removeEventListener("wheel", onWheel);
      this.canvas.removeEventListener("contextmenu", onContextMenu);
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("keyup", onKeyUp);
      window.removeEventListener("blur", onBlur);
    });
  }

  detach(): void {
    for (const cleanup of this.cleanups.splice(0)) cleanup();
    this.pressedKeys.clear();
    this.pressedModifiers.clear();
    if (document.pointerLockElement === this.canvas) {
      void document.exitPointerLock();
    }
  }

  private send(value: unknown): void {
    if (this.ws?.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify(value));
    }
  }

  private pointFromEvent(event: PointerEvent): { x: number; y: number; scale: number } {
    const rect = this.canvas.getBoundingClientRect();
    const width = this.surfaceW || this.canvas.width;
    const height = this.surfaceH || this.canvas.height;
    const scale = Math.min(rect.width / width, rect.height / height) || 1;
    const left = rect.left + (rect.width - width * scale) / 2;
    const top = rect.top + (rect.height - height * scale) / 2;
    return {
      x: clamp((event.clientX - left) / scale, 0, width - 1),
      y: clamp((event.clientY - top) / scale, 0, height - 1),
      scale,
    };
  }

  private onPointerDown(event: PointerEvent): void {
    if (event.button > 2) return;
    this.canvas.focus();
    this.canvas.setPointerCapture(event.pointerId);
    if (this.mouseMode !== MOUSE_MODE_CLIENT && document.pointerLockElement !== this.canvas) {
      // Grab the pointer for relative movement; the click is still forwarded.
      try {
        const request = this.canvas.requestPointerLock?.();
        if (request && typeof (request as Promise<void>).catch === "function") {
          (request as Promise<void>).catch(() => undefined);
        }
      } catch {
        // Pointer lock is optional; absolute positions still work.
      }
    }
    const button = event.button === 0 ? BUTTON_LEFT : event.button === 1 ? BUTTON_MIDDLE : BUTTON_RIGHT;
    this.buttons |= 1 << button;
    this.send({ type: "button", button, down: true });
    event.preventDefault();
  }

  private onPointerUp(event: PointerEvent): void {
    if (event.button > 2) return;
    if (this.canvas.hasPointerCapture(event.pointerId)) {
      this.canvas.releasePointerCapture(event.pointerId);
    }
    const button = event.button === 0 ? BUTTON_LEFT : event.button === 1 ? BUTTON_MIDDLE : BUTTON_RIGHT;
    this.buttons &= ~(1 << button);
    this.send({ type: "button", button, down: false });
    event.preventDefault();
  }

  private onPointerMove(event: PointerEvent): void {
    if (this.surfaceW === 0 || this.surfaceH === 0) return;
    if (this.mouseMode === MOUSE_MODE_CLIENT) {
      const point = this.pointFromEvent(event);
      this.virtualX = point.x;
      this.virtualY = point.y;
      this.send({ type: "mouse", x: Math.round(point.x), y: Math.round(point.y) });
      return;
    }
    // Server-side mouse: the backend expects motion, but accepts an absolute
    // position and derives the delta.  Pointer lock gives us raw movement.
    if (document.pointerLockElement === this.canvas) {
      const rect = this.canvas.getBoundingClientRect();
      const scale = Math.min(rect.width / this.surfaceW, rect.height / this.surfaceH) || 1;
      this.virtualX = clamp(this.virtualX + event.movementX / scale, 0, this.surfaceW - 1);
      this.virtualY = clamp(this.virtualY + event.movementY / scale, 0, this.surfaceH - 1);
    } else {
      const point = this.pointFromEvent(event);
      this.virtualX = point.x;
      this.virtualY = point.y;
    }
    this.send({
      type: "mouse",
      x: Math.round(this.virtualX),
      y: Math.round(this.virtualY),
    });
  }

  private onWheel(event: WheelEvent): void {
    event.preventDefault();
    const delta = event.deltaMode === 0 ? event.deltaY : event.deltaY * 40;
    this.wheelAccum += delta;
    const step = 100;
    if (Math.abs(this.wheelAccum) >= step) {
      const steps = clamp(Math.trunc(this.wheelAccum / step), -10, 10);
      this.wheelAccum -= steps * step;
      if (steps !== 0) this.send({ type: "wheel", delta: steps });
    }
  }

  private onKeyDown(event: KeyboardEvent): void {
    if (event.target instanceof HTMLElement) {
      const tag = event.target.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || event.target.isContentEditable) {
        return;
      }
    }
    if (event.code === "F11") return; // let the browser handle fullscreen
    if (!isMapped(event.code)) return;
    event.preventDefault();
    if (this.pressedKeys.has(event.code)) return;
    this.pressedKeys.add(event.code);
    if (event.code.startsWith("Shift") || event.code.startsWith("Control") ||
        event.code.startsWith("Alt") || event.code.startsWith("Meta")) {
      this.pressedModifiers.add(event.code);
      this.send({ type: "modifiers", modifiers: modifierMask(this.pressedModifiers) });
    }
    this.send({ type: "key", code: event.code, down: true });
  }

  private onKeyUp(event: KeyboardEvent): void {
    if (!this.pressedKeys.has(event.code)) return;
    event.preventDefault();
    this.pressedKeys.delete(event.code);
    this.send({ type: "key", code: event.code, down: false });
    if (this.pressedModifiers.delete(event.code)) {
      this.send({ type: "modifiers", modifiers: modifierMask(this.pressedModifiers) });
    }
  }

  private releaseAll(): void {
    for (const code of Array.from(this.pressedKeys)) {
      this.send({ type: "key", code, down: false });
    }
    this.pressedKeys.clear();
    this.pressedModifiers.clear();
    if (this.buttons !== 0) {
      for (const button of [BUTTON_LEFT, BUTTON_MIDDLE, BUTTON_RIGHT]) {
        if (this.buttons & (1 << button)) {
          this.send({ type: "button", button, down: false });
        }
      }
      this.buttons = 0;
    }
    this.send({ type: "modifiers", modifiers: 0 });
  }

  refresh(): void {
    this.send({ type: "refresh" });
  }

  sendCtrlAltDel(): void {
    this.send({ type: "modifiers", modifiers: (1 << 2) | (1 << 4) });
    const sequence: Array<[string, boolean]> = [
      ["ControlLeft", true],
      ["AltLeft", true],
      ["Delete", true],
      ["Delete", false],
      ["AltLeft", false],
      ["ControlLeft", false],
    ];
    sequence.forEach(([code, down], index) => {
      window.setTimeout(() => this.send({ type: "key", code, down }), index * 40);
    });
    window.setTimeout(() => this.send({ type: "modifiers", modifiers: 0 }), sequence.length * 40);
  }
}
