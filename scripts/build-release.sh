#!/usr/bin/env bash
# Build utm-web release archives.
#
# Usage:
#   ./scripts/build-release.sh                 # native + x86_64 Linux + x86_64 Windows
#   ./scripts/build-release.sh <target> [...]  # explicit Rust target triples
#   ./scripts/build-release.sh --no-cross      # native target only
#
# Cross builds use cargo-zigbuild and Zig 0.14.x. If they are unavailable the
# script warns and skips those targets instead of failing the native build.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

DIST="$ROOT/dist"
HOST="$(rustc -vV | sed -n 's/^host: //p')"

CROSS=1
TARGETS=()
for arg in "$@"; do
  case "$arg" in
    --no-cross) CROSS=0 ;;
    -h|--help)
      sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    -*)
      echo "unknown option: $arg" >&2
      exit 2
      ;;
    *) TARGETS+=("$arg") ;;
  esac
done

if [ "${#TARGETS[@]}" -eq 0 ]; then
  TARGETS=("$HOST")
  if [ "$CROSS" -eq 1 ]; then
    for t in x86_64-unknown-linux-gnu x86_64-pc-windows-gnu; do
      [ "$t" = "$HOST" ] || TARGETS+=("$t")
    done
  fi
fi

have() { command -v "$1" >/dev/null 2>&1; }

# The binary embeds web/dist. Rebuild it when the toolchain is available so a
# release never ships a stale UI.
if [ -d web/node_modules ] && have npm; then
  echo "==> building frontend"
  (cd web && npm run build)
elif [ -f web/dist/app.js ]; then
  echo "==> using committed web/dist (run 'npm ci && npm run build' in web/ to refresh)"
else
  echo "error: web/dist is missing and web/node_modules is not installed" >&2
  exit 1
fi

mkdir -p "$DIST"

platform_name() {
  case "$1" in
    *-unknown-linux-gnu) echo "linux" ;;
    *-pc-windows-*)      echo "windows" ;;
    *-apple-darwin)      echo "macos" ;;
    *)                   echo "unknown" ;;
  esac
}

arch_name() {
  case "$1" in
    x86_64-*)  echo "x86_64" ;;
    aarch64-*) echo "aarch64" ;;
    armv7-*)   echo "armv7" ;;
    *)         echo "unknown" ;;
  esac
}

build_target() {
  local target="$1"
  local out

  if [ "$target" = "$HOST" ]; then
    echo "==> building native target $target"
    cargo build --release --target "$target"
  elif have cargo-zigbuild && have zig; then
    echo "==> cross-building $target with cargo-zigbuild"
    cargo zigbuild --release --target "$target"
  else
    echo "warning: skipping $target (cargo-zigbuild and Zig are required)" >&2
    return 1
  fi

  out="$ROOT/target/$target/release"
  if [ -f "$out/utm-web.exe" ]; then
    BIN="$out/utm-web.exe"
  elif [ -f "$out/utm-web" ]; then
    BIN="$out/utm-web"
  else
    echo "error: build produced no binary for $target" >&2
    return 1
  fi
}

package_target() {
  local target="$1"
  local os arch name staging archive
  os="$(platform_name "$target")"
  arch="$(arch_name "$target")"
  name="utm-web-$os-$arch"
  staging="$DIST/$name"
  archive=""

  rm -rf "$staging"
  mkdir -p "$staging"
  cp "$BIN" "$staging/"
  cp README.md LICENSE BUILDING.md CONFIGURATION.md PROTOCOL.md \
    config.example.json "$staging/"

  case "$os" in
    windows)
      archive="$DIST/$name.zip"
      rm -f "$archive"
      (cd "$DIST" && zip -qr "$name.zip" "$name")
      ;;
    *)
      archive="$DIST/$name.tar.gz"
      tar -C "$DIST" -czf "$archive" "$name"
      ;;
  esac

  rm -rf "$staging"
  echo "==> wrote $archive"
  ARCHIVES+=("$archive")
}

ARCHIVES=()
for target in "${TARGETS[@]}"; do
  if build_target "$target"; then
    package_target "$target"
  fi
done

if [ "${#ARCHIVES[@]}" -eq 0 ]; then
  echo "error: no archives were produced" >&2
  exit 1
fi

echo "==> writing checksums"
(
  cd "$DIST"
  rm -f SHA256SUMS
  for a in "${ARCHIVES[@]}"; do
    sha256sum "$(basename "$a")"
  done > SHA256SUMS
)

echo
echo "Release artifacts in $DIST:"
ls -lh "$DIST" | sed '1d'
