# Frontend

The browser UI is plain TypeScript, HTML and CSS. There is no framework and no
runtime dependency: `esbuild` bundles `src/app.ts` and `src/style.css` into
`dist/`, and the Rust binary embeds those three files with `include_str!`.

```text
web/
  index.html        markup, dialog and toolbar
  src/app.ts        application state, API calls, event wiring
  src/api.ts        typed fetch/WebSocket helpers
  src/display.ts    canvas renderer, frame protocol, pointer/keyboard capture
  src/keyboard.ts   browser KeyboardEvent → SPICE scancode mapping
  src/style.css     responsive dark/light theme
  dist/             build output embedded by Rust (committed)
  build.mjs         esbuild script
```

## Build

```sh
npm ci
npm run build      # one-shot, minified
npm run watch      # rebuild on change (inline source maps)
npx tsc --noEmit   # type check
```

Run the frontend build before `cargo build` whenever you change anything under
`web/src`, `web/index.html` or `web/style.css`. The Rust build embeds
`web/dist`, so forgetting this step produces a binary with the previous UI.

For development, run `npm run watch` in one terminal and
`RUST_LOG=utm_web=debug cargo run` in another. The server does not proxy a
frontend dev server; it always serves the embedded bundle.

## Display protocol

The display WebSocket is:

```text
GET /api/display/{profile_id}/{vm_id}/ws
```

It starts with a JSON text frame:

```json
{"type":"ready","vm":"...","name":"...","width":1920,"height":1080}
```

After that the server sends binary frames and occasional JSON `{"type":"pong"}`
replies. Every binary message starts with a one-byte kind:

| Kind | Payload | Meaning |
| --- | --- | --- |
| `1` | `u32 LE x, y, w, h, surface_w, surface_h, seq; RGBA` | Dirty rectangle. |
| `2` | `u32 LE width, height` | Surface resized; clear the canvas. |
| `3` | UTF-8 text | Informational notice. |
| `4` | UTF-8 text | Display error. |
| `5` | `u16 LE mode` | Pointer mode (`1` server/relative, `2` client/absolute). |
| `6` | — | SPICE session closed. |

All integers are little-endian. The frame payload after the 29-byte header is
tightly packed RGBA, row-major, top down, `w * h * 4` bytes. The client applies
rectangles in order. A non-zero `seq` that is not the previous value plus one
means a dirty rectangle was missed, so the client sends `refresh`. Sequence `0`
marks a full-surface snapshot (initial connect, lag recovery or refresh) and
resets the sequence baseline.

Client → server messages are JSON text frames:

```json
{ "type": "key", "code": "KeyA", "down": true }
{ "type": "mouse", "x": 640, "y": 360 }
{ "type": "button", "button": 0, "down": true }
{ "type": "wheel", "delta": -1 }
{ "type": "modifiers", "modifiers": 5 }
{ "type": "refresh" }
{ "type": "ping" }
```

`key.code` is a browser `KeyboardEvent.code` value; the server maps it to a PC
XT set-1 scancode. `button` is 0 = left, 1 = middle, 2 = right. Positive wheel
`delta` scrolls down. In absolute pointer mode `mouse.x`/`mouse.y` are surface
coordinates; in relative mode the client sends accumulated virtual coordinates
and the server derives the deltas. The UI updates its pointer hint from kind
`5` messages.

## Browser support

The UI targets Chrome 100+, Firefox 102+ and Safari 15+. It uses Canvas 2D,
WebSocket, `<dialog>`, Pointer Lock and `ResizeObserver`. Pointer Lock requires
`localhost` or HTTPS; everything else works over plain HTTP on a trusted LAN.

See the main [README](../README.md#browser-limitations) for the list of SPICE
features that are intentionally not implemented (audio, clipboard, USB,
multi-monitor, hardware video).
