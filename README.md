# utm-web

A small, self-hosted web client for [Apple UTM](https://mac.getutm.app/) Remote.
It reimplements the UTM Remote protocol and speaks SPICE directly, so a normal
browser can list, control and view macOS/Linux/Windows virtual machines running
on a Mac — without installing anything on the browser side.

`utm-web` is a single Rust executable. It serves the web UI, the HTTP API and the
WebSocket streams, and it keeps the UTM client certificate private on the
server. The frontend is plain TypeScript, HTML and CSS.

> This project is not affiliated with Apple or the UTM project. UTM Remote is a
> feature of UTM; you need a Mac running UTM with Remote access enabled.

## Features

- **Profiles** for multiple UTM servers, stored server-side.
- **VM lifecycle**: start, stop, force stop, restart, pause and resume.
- **Live display** over SPICE with dirty-rectangle updates, resize handling and
  JPEG/bitmap decoding.
- **Keyboard and mouse** forwarding, including absolute and relative pointer
  modes, wheel and modifier state.
- **Fullscreen**, reconnect, full-redraw and Ctrl+Alt+Del shortcuts.
- **Certificate trust flow**: the first connection shows the UTM connection
  fingerprint; trusting it pins the fingerprint for later connections.
- **Mutual TLS** to UTM with the pyutmremote-compatible client certificate
  format (including encrypted PKCS#8 keys).
- **SPICE TLS pinning** against the `spicePublicKey` advertised by UTM.
- **Multiple browser viewers** can share one SPICE display session; idle
  sessions are reaped automatically.
- **No database, Redis, Docker or external services.** One process, one JSON
  config file.

## Requirements

On the Mac:

- UTM with **Remote access** enabled (UTM → Settings → Remote).
- The UTM Remote port (default `21589`) reachable from the machine running
  `utm-web`.
- A user account on the Mac and, if required, the UTM Remote password.

On the machine running `utm-web`:

- Linux, macOS or Windows.
- No special privileges. The default listen address is `127.0.0.1:8080`.

On the browser:

- A current Chromium, Firefox or Safari with Canvas 2D, WebSocket and
  `<dialog>` support. See [Browser limitations](#browser-limitations).

## Quick start

Download a release archive for your platform, or build from source (see
[BUILDING.md](BUILDING.md)).

```sh
chmod +x utm-web
./utm-web
```

Open <http://localhost:8080>, click **Add**, and enter:

| Field | Value |
| --- | --- |
| Host | Hostname or IP of the Mac running UTM |
| Port | `21589` (UTM Remote default) |
| Password | The UTM Remote password, if one is set |

Click **Connect**. On the first connection the server certificate is untrusted;
compare the displayed fingerprint with the one shown in UTM on the Mac, then
click **Trust server**. The VM list appears once the handshake succeeds.

The first run generates a client certificate at
`~/.config/utm-web/client.pem` (or `%APPDATA%\utm-web\client.pem` on Windows).
Keep this file: it identifies this installation to UTM.

## Command line

```text
utm-web [OPTIONS]

  --listen <ADDR>          Listen address (default 127.0.0.1:8080)
  --config <FILE>          Configuration file
  --cert <FILE>            Client certificate PEM
  --cert-password <PASS>   Password for an encrypted PKCS#8 client key
  --generate-cert          Generate the client certificate and exit
  -h, --help               Show help
  -V, --version            Show version
```

Everything can also be configured with environment variables or the JSON config
file. See [CONFIGURATION.md](CONFIGURATION.md).

## How it works

```text
browser ──HTTP/WebSocket──▶ utm-web ──mutual TLS──▶ UTM Remote (SwiftConnect)
                               │
                               └──TLS + SPICE──▶ QEMU SPICE server
```

- The browser never sees the UTM client certificate, the UTM password or the
  SPICE ticket. All of that stays in the Rust process.
- UTM traffic uses the SwiftConnect framing and Apple `Codable` binary payloads.
- Display traffic uses the real SPICE protocol: link handshake, ticket
  authentication, main/display/inputs channels, mini headers, ACKs and draw
  commands. It is not a WebSocket-only approximation.
- Dirty rectangles are coalesced for a few milliseconds and sent as binary
  WebSocket messages containing RGBA pixels.
- One SPICE session is shared by all viewers of the same profile and VM. Input
  from any viewer is forwarded to the guest.

The wire details are documented in [PROTOCOL.md](PROTOCOL.md).

## Security

- `utm-web` has **no built-in user authentication**. The default listen address
  is loopback-only. If you expose it, put it behind a reverse proxy that
  provides TLS and authentication, and set `public_url` accordingly.
- UTM passwords are stored in the config file in plain text (mode `0600` on
  Unix). Protect that file; it is equivalent to a credential store.
- UTM server certificates are pinned by the XOR connection fingerprint used by
  pyutmremote. A changed fingerprint is refused until you explicitly re-trust
  it.
- SPICE server certificates are pinned by SubjectPublicKeyInfo against the key
  returned by UTM.
- The web UI uses a restrictive Content Security Policy and loads no third-party
  resources.

## Browser limitations

The browser client is intentionally small. The following are **not** available:

- Audio output (SPICE playback channel).
- Clipboard sharing and drag-and-drop (SPICE agent / clipboard channel).
- USB redirection, camera and smartcard passthrough.
- Multiple monitors (the primary surface is shown; monitor configuration
  messages are accepted but not rendered as separate displays).
- Hardware-accelerated video streams (H.264/VP8). `utm-web` advertises
  compression `OFF`; if a guest/server ignores that, some frames may not render
  until a redraw.
- Some browser-reserved shortcuts cannot be forwarded (for example Ctrl+W,
  Ctrl+T, Ctrl+N and the browser's own fullscreen key). Use the toolbar buttons
  where possible.

**Smallest practical workaround for clipboard/audio/USB:** use UTM's own GUI on
the Mac for those features, or run a local SPICE client (for example
`remote-viewer`) against the same VM. Everything else — display, keyboard,
pointer and lifecycle — works in the browser.

Pointer capture uses the Pointer Lock API. It is available on `localhost` and
HTTPS origins; on a plain-HTTP LAN origin, relative pointer mode still works but
the pointer is not captured.

## Documentation

- [BUILDING.md](BUILDING.md) — build, cross-compile, install, release.
- [CONFIGURATION.md](CONFIGURATION.md) — config file, environment, CLI, proxy.
- [PROTOCOL.md](PROTOCOL.md) — UTM Remote and SPICE compatibility notes.
- [web/README.md](web/README.md) — frontend layout and build.

## Tests

```sh
cargo test
```

The suite includes byte-for-byte Codable fixtures from pyutmremote, a mock UTM
server that exercises mutual TLS, framing, fingerprints and VM listing, and
SPICE display parser/render tests.

## License

MIT. See [LICENSE](LICENSE).
