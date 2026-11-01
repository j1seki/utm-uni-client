# Protocol notes

This document describes what `utm-web` implements. It is a compatibility note,
not an official specification. The authoritative references used while building
it were:

- pyutmremote's `utmremote/` Python package (Codable, framing, fingerprints).
- Apple's UTM Remote interface (SwiftConnect).
- `spice-protocol` and `spice-gtk` for SPICE framing and channels.

## 1. UTM Remote (SwiftConnect)

### Transport

- TCP to the UTM Remote port (default `21589`).
- TLS 1.2 or newer. The client presents a self-signed certificate; UTM presents
  a self-signed certificate. Certificate-chain validation is intentionally not
  used; trust is established with the fingerprint scheme below.
- Every frame is an 8-byte big-endian unsigned length followed by that many
  bytes.

### Frame payload

```text
u8      message id
u8      flags
uleb128 token
bytes   Codable body
```

Flags:

| Bit | Name | Meaning |
| --- | --- | --- |
| `0x01` | response | This frame answers a request with the same token. |
| `0x02` | error | The response body is a UTF-8 error string, not Codable. |

Client requests use a monotonically increasing token starting at 1. Replies
carry the request token. Server-initiated requests use their own token and must
be answered with the same token and the response flag.

The handshake (`id = 0`) is the only message accepted before the connection is
marked trusted. After a successful handshake, server messages are dispatched.

### Message IDs

Requests sent by `utm-web` to UTM:

| ID | Message |
| --- | --- |
| 0 | handshake |
| 1 | list virtual machines |
| 2 | reorder virtual machines |
| 3 | get virtual machine information |
| 4 | get QEMU configuration |
| 5 | get package size |
| 6 | get package file |
| 7 | send package file |
| 8 | delete package file |
| 9 | mount guest tools |
| 10 | start virtual machine |
| 11 | stop virtual machine |
| 12 | restart virtual machine |
| 13 | pause virtual machine |
| 14 | resume virtual machine |
| 15 | save snapshot |
| 16 | delete snapshot |
| 17 | restore snapshot |
| 18 | change pointer type |

Requests sent by UTM to `utm-web`:

| ID | Message |
| --- | --- |
| 0 | client handshake |
| 1 | VM list changed |
| 2 | QEMU configuration changed |
| 3 | mounted drives changed |
| 4 | VM did transition |
| 5 | VM did error |

`utm-web` answers every server request, including ones it does not act on, so
UTM's request/reply machinery does not stall.

### Codable payloads

UTM uses Apple's binary `Codable` representation. The implementation follows
pyutmremote's `cod.py`:

```text
uleb128 shape_count
shape_count × {
    uleb128 key_count
    key_count × { uleb128 key_length, key_bytes }
}
value
```

Keyed containers (structs, dictionaries, enum cases):

```text
u8      metadata: 0 = non-null, 1 = nullable
uleb128 shape id
[nullable: ceil(key_count / 8) bytes of null flags]
uleb128 encoded length of each non-null value
value bytes...
```

Unkeyed containers (arrays, bytes):

```text
u8      metadata: 0 = fixed size, 1 = heterogeneous, 2 = nullable
uleb128 count
[0: uleb128 element size] or [1: uleb128 size per element]
value bytes...
```

Other rules:

- Integers are encoded at their Swift type width, little-endian. `Int` is 8
  bytes; narrower types such as `UInt16` use 2 bytes. The decoder sign-extends
  values shorter than 8 bytes.
- Strings are ULEB128 byte length followed by UTF-8.
- Swift enums with a raw value (`enum X: Int, Codable` or `enum X: String,
  Codable`) encode as a single `Int` or `String`, **not** as a keyed
  container. `UTMVirtualMachineState` and `UTMBackend` are examples.
- Swift enums without a raw value encode as a keyed container with exactly one
  key (the case name). The decoder returns that key as a string.
- Swift `OptionSet` structs encode as their bare raw integer, **not** as a
  keyed container. `UTMCapabilities` and `UTMVirtualMachineStartOptions` are
  examples; this matches the `IntFlag` model used by pyutmremote and was
  confirmed against a captured `ServerHandshake.Reply` (`capabilities` was 8
  bytes of `0x07`).
- Dictionaries and structs use sorted keys, matching pyutmremote's encoder.
- `Type::Skip` exists for opaque values such as QEMU configurations.

The unit tests contain byte-for-byte fixtures generated with pyutmremote, for
example:

```text
handshake {version:1,password:"secret"}
01020870617373776f72640776657273696f6e00000708067365637265740100000000000000
```

### Handshake and authentication

Client handshake body: `{version: 1, password?: string}`.

Reply: `{version, isAuthenticated, capabilities, model}`, where
`capabilities` is the `UTMCapabilities` OptionSet encoded as a bare integer.
If `isAuthenticated` is false, the connection is rejected
(`password_required` if no password was sent, `invalid_password` otherwise).

### Fingerprints

Each side computes SHA-256 over its DER certificate. The *connection
fingerprint* is the byte-wise XOR of the server and client fingerprints:

```text
connection[i] = sha256(server_der)[i] XOR sha256(client_der)[i]
```

It is rendered as 32 uppercase hex bytes separated by colons:

```text
AA:BB:CC:...
```

The profile stores this value. A mismatch is refused. This is exactly the
scheme used by pyutmremote and shown in UTM's Remote settings.

## 2. SPICE

UTM starts a SPICE server for a VM and returns a `serverInfo` object:

| Field | Meaning |
| --- | --- |
| `spicePortInternal` | Port reachable from the UTM host. |
| `spicePortExternal` | Externally advertised port, if configured. |
| `spiceHostExternal` | Externally advertised host. |
| `spicePublicKey` | DER SubjectPublicKeyInfo of the SPICE TLS certificate. |
| `spicePassword` | One-time SPICE ticket. |

`utm-web` prefers the external endpoint and falls back to the UTM server host
plus the internal port, matching pyutmremote's GUI.

### TLS and pinning

SPICE is wrapped in TLS. The server certificate is accepted only if its
SubjectPublicKeyInfo DER equals `spicePublicKey`. There is no client certificate
for SPICE.

### Link handshake

Client header (16 bytes):

```text
"REDQ" (4)
u32 LE major = 2
u32 LE minor = 2
u32 LE link message size
```

Link message (18 bytes + capability arrays):

```text
u32 LE connection id
u8     channel type   (1 = main, 2 = display, 3 = inputs)
u8     channel id
u32 LE number of common capabilities
u32 LE number of channel capabilities
u32 LE capability offset (18)
u32 LE[] common capabilities
u32 LE[] channel capabilities
```

Server reply: 16-byte header followed by a body of at least 178 bytes:

```text
u32 LE link error (0 = OK)
162 bytes RSA public key (DER SubjectPublicKeyInfo)
u32 LE number of common capabilities
u32 LE number of channel capabilities
u32 LE capability offset
capability words...
```

### Authentication

If the peer advertises `PROTOCOL_AUTH_SELECTION`, the client selects
`AUTH_SPICE` (`u32 LE = 1`). The ticket is:

```text
RSA-OAEP(SHA-1, password_utf8 || 0x00)
```

The encrypted ticket is exactly 128 bytes (RSA-1024). The server replies with a
`u32 LE` auth result; `0` means success.

### Capabilities and framing

Common capabilities advertised by `utm-web`:

- protocol auth selection
- SPICE ticket auth
- mini header

Display channel capabilities:

- monitor configuration
- preferred compression

If the mini-header capability is negotiated, messages are framed as:

```text
u16 LE type
u32 LE body size
body
```

Otherwise the 18-byte SPICE data header is used (serial, type, size, flags).

### Base channel messages

Server → client: `SET_ACK`, `PING`, `DISCONNECTING`, `NOTIFY`, `LIST`.

Client → server: `ACK_SYNC`, `ACK`, `PONG`.

`utm-web` replies to `PING` with `PONG`, synchronises the ACK generation and
sends periodic `ACK`s when the server requests them.

### Main channel

The main channel uses `connection_id = 0` and waits for `MAIN_INIT`:

```text
u32 session_id
u32 display_channels_hint
u32 supported_mouse_modes
u32 current_mouse_mode
u32 agent_connected
u32 agent_tokens
u32 multi_media_time
u32 ram_hint
```

After `MAIN_INIT`, `utm-web` sends `MSGC_MAIN_ATTACH_CHANNELS` and requests
client (absolute) mouse mode when the server supports it. The session id is used
as the `connection_id` for the display and inputs channels.

### Display channel

Initialization (`MSGC_DISPLAY_INIT`):

```text
u8  pixmap cache id = 1
i64 cache size = 4 MiB
u8  GLZ dictionary id = 1
i32 GLZ window = 0
```

Then `MSGC_DISPLAY_PREFERRED_COMPRESSION` with `OFF`. This asks the server to
send uncompressed bitmaps instead of QUIC/LZ/GLZ images. JPEG and zlib-GLZ are
also decoded if the server sends them.

Supported server messages:

| Type | Message |
| --- | --- |
| 101 | display mode |
| 102 | mark |
| 103 | reset |
| 104 | copy bits |
| 302 | draw fill |
| 303 | draw opaque |
| 304 | draw copy |
| 305 | draw blend |
| 306 | draw blackness |
| 307 | draw whiteness |
| 308 | draw invers |
| 309 | draw rop3 |
| 312 | draw transparent |
| 313 | draw alpha blend |
| 314 | surface create |
| 315 | surface destroy |

Every draw starts with a base:

```text
u32 surface_id
rect box     (i32 top, left, bottom, right — wire order!)
u8 clip type (0 = none, 1 = rectangles)
[clip type 1: u32 count, count × rect]
```

Note that the wire order is `top, left, bottom, right` while the in-memory
`SpiceRect` order in C is different. `utm-web` parses the wire order explicitly.

Image references are 32-bit offsets relative to the start of the SPICE message.
The pointed-to image descriptor is appended after the inline draw data and
aligned to four bytes.

Bitmap images:

```text
u64 id
u8  type = 0
u8  image flags
u32 width
u32 height
u8  format
u8  bitmap flags
u32 x
u32 y
u32 stride (signed)
palette id or palette offset
stride × height pixel bytes
```

Pixel formats:

- 32BIT/RGBA: bytes `B, G, R, X/A`.
- 24BIT: bytes `B, G, R`.
- 16BIT: RGB555 little-endian.
- 8BIT: palette index; palette entries are `u32` `0x00RRGGBB`.
- 1/4BIT: packed palette indices.

Rows are bottom-up unless `BITMAP_FLAG_TOP_DOWN` is set.

The renderer keeps RGBA8 canvases in memory, applies clipping, masks, solid and
pattern brushes, and emits dirty rectangles.

### Inputs channel

Client messages:

| Type | Message |
| --- | --- |
| 101 | key down |
| 102 | key up |
| 103 | key modifiers |
| 104 | key scancode |
| 111 | mouse motion |
| 112 | mouse position |
| 113 | mouse press |
| 114 | mouse release |

`utm-web` maps browser `KeyboardEvent.code` values to PC XT set-1 scancodes.
Extended keys are represented as `0x100 + code` and converted to the `0xe0`
prefixed form. Mouse button masks: left 1, middle 2, right 4, up 8, down 16,
side 32, extra 64.

## 3. Browser WebSocket protocols

### Agent events

`GET /api/profiles/{id}/events` upgrades to a text WebSocket. The server sends:

```json
{"type":"hello","model":"MacBookPro18,3","connection_fingerprint":"AA:...","vms":[...]}
{"type":"vms","vms":[...]}
{"type":"vm","vm":{...}}
{"type":"error","id":"...","message":"..."}
{"type":"disconnected","reason":"..."}
```

The browser may send `{"type":"ping"}`; the server replies `{"type":"pong"}`.
VM lifecycle events are pushed; there is no polling.

### Display

`GET /api/display/{profile}/{vm}/ws` upgrades to a binary WebSocket. Text
messages are JSON control messages; binary messages start with a one-byte kind:

| Kind | Payload |
| --- | --- |
| 1 | frame: `u32 LE x, y, w, h, surface_w, surface_h, seq`, then RGBA8 pixels |
| 2 | resize: `u32 LE width, height` |
| 3 | notice: UTF-8 text |
| 4 | error: UTF-8 text |
| 5 | mouse mode: `u16 LE` (1 = server/relative, 2 = client/absolute) |
| 6 | closed |

The first text message is `{"type":"ready","vm":...,"name":...,"width":...,"height":...}`.
The server also sends a full-surface frame and the current mouse mode on connect.
Frames with `seq = 0` are full-surface snapshots; other frames carry the
monotonically increasing sequence number used to detect dropped rectangles.

Client text messages:

```json
{"type":"key","code":"KeyA","down":true}
{"type":"mouse","x":123,"y":456}
{"type":"button","button":0,"down":true}
{"type":"wheel","delta":-1}
{"type":"modifiers","modifiers":5}
{"type":"refresh"}
{"type":"ping"}
```

`button` is 0 = left, 1 = middle, 2 = right. Positive wheel delta scrolls down.

## 4. Compatibility tests

`tests/utm_mock.rs` starts a TLS server that:

- requires a client certificate,
- speaks the 8-byte framing and Codable handshake,
- verifies the client handshake body,
- returns a VM list and VM information.

The tests cover a trusted connection, the untrusted fingerprint flow, a
fingerprint mismatch and a rejected password. Display parser tests build raw
SPICE messages and assert the rendered pixels.

## 5. Known gaps

- SPICE video streams (H.264/VP8/QUIC) are not decoded. Compression `OFF` is
  requested to avoid them.
- GLZ/LZ/LZ4 palette images are not decoded. zlib-GLZ and JPEG are.
- Only the primary display surface is streamed to the browser. Additional
  surfaces are kept for draw operations but not shown as separate monitors.
- SPICE audio, clipboard, USB and smartcard channels are not implemented.
- UTM package transfer (VM import/export) messages exist in the protocol module
  but are not exposed in the web UI.
