#!/usr/bin/env bash
# Baut horizon-server und das Spiel (Server-Build ohne Grafik/Sound) aus dem Quellcode von
# github.com/horizon-rust/horizon – für den Raspberry Pi gibt es keine fertigen Downloads.
# Wird vom Panel („Horizon bauen“) oder von install.sh aufgerufen.
set -euo pipefail

DATA="${HORIZON_PANEL_DIR:-$HOME/horizon-panel}"
SRC="$DATA/horizon-src"
BIN="$DATA/bin"
REPO="https://github.com/horizon-rust/horizon.git"
BRANCH="${HORIZON_BRANCH:-main}"

say() { echo "==> $*"; }

[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
command -v cargo >/dev/null || { echo "Rust (cargo) fehlt – erst install.sh ausführen."; exit 1; }

# Horizon braucht ein aktuelles Rust (mindestens das, was sein Cargo.toml verlangt).
if command -v rustup >/dev/null; then
  say "Rust aktualisieren"
  rustup update stable --no-self-update 2>&1 | tail -n 2 || true
fi
say "$(rustc --version)"

FREE_GB=$(df -Pk "$DATA" | awk 'NR==2 { print int($4 / 1048576) }')
if [ "$FREE_GB" -lt 12 ]; then
  echo "Nur $FREE_GB GB frei – der Build braucht etwa 12 GB. Bitte Platz schaffen."
  exit 1
fi

mkdir -p "$DATA" "$BIN"
if [ -d "$SRC/.git" ]; then
  say "Neuesten Horizon-Code holen ($BRANCH)"
  git -C "$SRC" fetch --depth 1 origin "$BRANCH"
  git -C "$SRC" reset --hard FETCH_HEAD
else
  say "Horizon-Code herunterladen"
  git clone --depth 1 --branch "$BRANCH" "$REPO" "$SRC"
fi
say "Stand: $(git -C "$SRC" log -1 --format='%h %cs %s')"

# Auf Pis mit wenig RAM weniger parallel bauen, sonst geht der Speicher aus.
MEM_GB=$(awk '/MemTotal/ { print int($2 / 1048576) }' /proc/meminfo)
JOBS=$(nproc)
if [ "$MEM_GB" -lt 6 ]; then JOBS=2; fi
if [ "$MEM_GB" -lt 3 ]; then JOBS=1; fi
say "Baue mit $JOBS Kernen (RAM: ${MEM_GB} GB). Das dauert – Zeit für einen Kaffee."

cd "$SRC"
START=$(date +%s)
# Das Spiel als Server-Build: ohne Sound und Controller (braucht weder ALSA noch udev).
cargo build --release --locked -j "$JOBS" -p horizon-client --no-default-features --bin horizon-client
cargo build --release --locked -j "$JOBS" -p horizon-server --bin horizon-server

say "Programme nach $BIN kopieren"
# Erst neben die alten kopieren, dann umbenennen: laufende Server stören sich nicht daran.
install -m 755 target/release/horizon-client "$BIN/horizon-engine.new"
install -m 755 target/release/horizon-server "$BIN/horizon-server.new"
mv -f "$BIN/horizon-engine.new" "$BIN/horizon-engine"
mv -f "$BIN/horizon-server.new" "$BIN/horizon-server"

MIN=$(( ($(date +%s) - START) / 60 ))
say "Fertig nach $MIN Minuten: $("$BIN/horizon-server" version 2>/dev/null | head -n 1)"
