#!/usr/bin/env bash
# Baut Horizon am PC für den Raspberry Pi (ARM64) und kopiert es auf Wunsch gleich rüber.
# Läuft unter Linux oder in WSL (Ubuntu) auf Windows. Viel schneller als auf dem Pi zu bauen.
#
#   ./scripts/cross-build-horizon.sh                          # nur bauen → ./horizon-pi-bin/
#   ./scripts/cross-build-horizon.sh --pi pi@raspberrypi.local  # bauen und auf den Pi kopieren
#
# Gebaut wird mit cargo-zigbuild gegen glibc 2.36, damit es auf Raspberry Pi OS Bookworm und
# neuer läuft (ein normales Ubuntu würde gegen eine zu neue glibc linken).
set -euo pipefail

PI=""
BRANCH="${HORIZON_BRANCH:-main}"
SRC="${HORIZON_SRC:-$HOME/horizon-src}"
OUT="$(pwd)/horizon-pi-bin"
REMOTE_DATA="horizon-panel"   # relativ zum Home-Ordner auf dem Pi
TARGET="aarch64-unknown-linux-gnu"
GLIBC="2.36"

while [ $# -gt 0 ]; do
  case "$1" in
    --pi) PI="${2:?--pi braucht benutzer@adresse}"; shift 2 ;;
    --branch) BRANCH="${2:?}"; shift 2 ;;
    -h|--help) sed -n '2,10p' "$0"; exit 0 ;;
    *) echo "Unbekannte Option $1"; exit 1 ;;
  esac
done

say() { printf '\n\033[1;32m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31mFehler:\033[0m %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = Linux ] || die "Bitte unter Linux oder WSL ausführen (Windows: wsl --install -d Ubuntu)."

if ! command -v cc >/dev/null || ! command -v git >/dev/null || ! command -v python3 >/dev/null; then
  say "Pakete installieren"
  sudo apt-get update -qq
  sudo apt-get install -y -qq build-essential git curl pkg-config python3 python3-venv ca-certificates >/dev/null
fi

if [ ! -x "$HOME/.cargo/bin/rustup" ]; then
  say "Rust installieren"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
fi
# shellcheck disable=SC1091
. "$HOME/.cargo/env"
rustup update stable --no-self-update >/dev/null 2>&1 || true
rustup target add "$TARGET" >/dev/null

# Zig als Cross-Linker (aus PyPI, eigenes venv, nichts systemweit).
if ! command -v zig >/dev/null; then
  say "Zig installieren"
  python3 -m venv "$HOME/.horizon-zig"
  "$HOME/.horizon-zig/bin/pip" install -q ziglang
  mkdir -p "$HOME/.local/bin"
  printf '#!/bin/sh\nexec "%s/.horizon-zig/bin/python" -m ziglang "$@"\n' "$HOME" > "$HOME/.local/bin/zig"
  chmod +x "$HOME/.local/bin/zig"
  export PATH="$HOME/.local/bin:$PATH"
fi
command -v cargo-zigbuild >/dev/null || { say "cargo-zigbuild installieren"; cargo install --locked cargo-zigbuild; }

if [ -d "$SRC/.git" ]; then
  say "Neuesten Horizon-Code holen ($BRANCH)"
  git -C "$SRC" fetch --depth 1 origin "$BRANCH"
  git -C "$SRC" reset --hard FETCH_HEAD
else
  say "Horizon-Code herunterladen"
  git clone --depth 1 --branch "$BRANCH" https://github.com/horizon-rust/horizon.git "$SRC"
fi
say "Stand: $(git -C "$SRC" log -1 --format='%h %cs %s')"

say "Für den Pi bauen ($TARGET, glibc $GLIBC)"
START=$(date +%s)
cd "$SRC"
cargo zigbuild --release --locked --target "$TARGET.$GLIBC" -p horizon-client --no-default-features --bin horizon-client
cargo zigbuild --release --locked --target "$TARGET.$GLIBC" -p horizon-server --bin horizon-server
cd - >/dev/null

mkdir -p "$OUT"
cp "$SRC/target/$TARGET/release/horizon-client" "$OUT/horizon-engine"
cp "$SRC/target/$TARGET/release/horizon-server" "$OUT/horizon-server"
chmod 755 "$OUT"/*
say "Fertig nach $(( ($(date +%s) - START) / 60 )) Minuten: $OUT"
file "$OUT/horizon-server" 2>/dev/null || true

if [ -z "$PI" ]; then
  echo
  echo "Auf den Pi kopieren: dieses Skript mit --pi benutzer@adresse starten, oder von Hand:"
  echo "  scp $OUT/horizon-* benutzer@raspberrypi.local:~/$REMOTE_DATA/bin/"
  exit 0
fi

say "Auf $PI kopieren"
ssh "$PI" "mkdir -p ~/$REMOTE_DATA/bin"
scp "$OUT/horizon-engine" "$PI:$REMOTE_DATA/bin/horizon-engine.new"
scp "$OUT/horizon-server" "$PI:$REMOTE_DATA/bin/horizon-server.new"
# Erst daneben ablegen, dann umbenennen: laufende Server stören sich nicht daran.
ssh "$PI" "cd ~/$REMOTE_DATA/bin && chmod 755 *.new && mv -f horizon-engine.new horizon-engine && mv -f horizon-server.new horizon-server && ./horizon-server version"
say "Auf dem Pi installiert. Laufende Server im Panel einmal neu starten."
