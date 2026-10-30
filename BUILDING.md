# Building and installing

`utm-web` is a single Rust executable with an embedded frontend. There are no
runtime dependencies beyond the operating system's C library (Linux), the
system libraries shipped with Windows, or macOS itself.

## Prerequisites

| Tool | Version | Notes |
| --- | --- | --- |
| Rust | 1.80 or newer (stable) | Install with [rustup](https://rustup.rs/). |
| Node.js | 18 or newer | Only needed when changing the frontend. |
| Zig | 0.14.x | Only for cross-compilation with `cargo-zigbuild`. |

Linux builds use `ring` for TLS and need a C compiler for the `ring` assembly
(`cc`/`gcc`). A normal `build-essential` / `gcc` package is enough. No OpenSSL
development package is required.

## Build from source

```sh
git clone <repository-url> utm-web
cd utm-web

# The frontend is embedded at compile time. Rebuild it after changing web/src.
(cd web && npm ci && npm run build)

cargo build --release
./target/release/utm-web
```

`web/dist/` is committed, so a checkout that does not change the UI can skip
the Node.js step. The build fails with a clear `include_str!` error if
`web/dist/` is missing.

Open <http://localhost:8080>.

### Development loop

```sh
# Terminal 1: rebuild the frontend on change
(cd web && npm run watch)

# Terminal 2: run the server with debug logging
RUST_LOG=utm_web=debug cargo run
```

For frontend type checking:

```sh
(cd web && npx tsc --noEmit)
```

## Tests

```sh
cargo test
cargo clippy --all-targets
```

The tests include:

- byte-for-byte Swift `Codable` fixtures taken from pyutmremote;
- a mock UTM server that performs mutual TLS, framing, password handshake,
  fingerprint trust and VM listing (`tests/utm_mock.rs`);
- SPICE display parser, bitmap/JPEG and rectangle-order tests;
- certificate fingerprint tests.

An end-to-end test against a real UTM installation is not included because it
requires macOS, UTM and a configured VM. Use a real Mac for a final smoke test.

## Cross-compilation

The release archives for Linux x86_64 and Windows x86_64 are produced from
Linux with [Zig](https://ziglang.org/) and
[`cargo-zigbuild`](https://github.com/rust-cross/cargo-zigbuild). Zig supplies
a self-contained C toolchain, which is what `ring` needs.

```sh
rustup target add x86_64-unknown-linux-gnu x86_64-pc-windows-gnu aarch64-unknown-linux-gnu
cargo install cargo-zigbuild --version 0.24.0 --locked

# Put a Zig 0.14.x binary on PATH, then:
cargo zigbuild --release --target x86_64-unknown-linux-gnu
cargo zigbuild --release --target x86_64-pc-windows-gnu
cargo zigbuild --release --target aarch64-unknown-linux-gnu
```

Outputs:

```text
target/x86_64-unknown-linux-gnu/release/utm-web
target/aarch64-unknown-linux-gnu/release/utm-web
target/x86_64-pc-windows-gnu/release/utm-web.exe
```

The Windows executable is a normal console program. It does not need a Visual
C++ redistributable beyond the Universal C Runtime that ships with Windows 10
and later.

### macOS

macOS binaries must be built on a macOS host (or a macOS CI runner) because the
SDK and linker are not redistributable:

```sh
rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin
lipo -create -output utm-web \
  target/aarch64-apple-darwin/release/utm-web \
  target/x86_64-apple-darwin/release/utm-web
```

## Release archives

`scripts/build-release.sh` builds the native target and, when `cargo-zigbuild`
and Zig are available, the cross targets. It writes archives and SHA-256
checksums to `dist/`:

```sh
./scripts/build-release.sh
```

Result:

```text
dist/utm-web-linux-aarch64.tar.gz
dist/utm-web-linux-x86_64.tar.gz
dist/utm-web-windows-x86_64.zip
dist/SHA256SUMS
```

Each archive contains the executable, `README.md`, `LICENSE`,
`BUILDING.md`, `CONFIGURATION.md`, `PROTOCOL.md` and `config.example.json`.
Build a specific target only:

```sh
./scripts/build-release.sh x86_64-unknown-linux-gnu
```

## Install

### Linux / macOS

```sh
install -Dm755 utm-web ~/.local/bin/utm-web
utm-web
```

For a system-wide install, copy the binary to `/usr/local/bin/utm-web` and use
the systemd unit in [CONFIGURATION.md](CONFIGURATION.md).

### Windows

Copy `utm-web.exe` anywhere and run it from PowerShell or `cmd.exe`. To start it
at login, create a shortcut in `shell:startup`, or register it with Task
Scheduler / [NSSM](https://nssm.cc/). The config directory defaults to
`%APPDATA%\utm-web`.

## Verifying a release

```sh
sha256sum -c SHA256SUMS
./utm-web --version
./utm-web --help
```

The first run creates `client.pem` in the config directory. Back that file up:
UTM identifies this installation by its client certificate.
