#!/usr/bin/env bash
# Installiert horizon-pi-panel auf dem Raspberry Pi (Raspberry Pi OS 64-bit).
#   git clone https://github.com/Lutziano888/Minecraft-Rust-Server-Panel.git && cd Minecraft-Rust-Server-Panel && ./install.sh
# Nochmal ausführen = Panel aktualisieren (Server, Welten und Passwort bleiben).
set -euo pipefail

cd "$(dirname "$0")"
REPO_DIR="$(pwd)"
USER_NAME="$(id -un)"
DATA="${HORIZON_PANEL_DIR:-$HOME/horizon-panel}"
SERVICE=horizon-pi-panel

say()  { printf '\n\033[1;32m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!\033[0m %s\n' "$*"; }
die()  { printf '\033[1;31mFehler:\033[0m %s\n' "$*" >&2; exit 1; }

[ "$(id -u)" -ne 0 ] || die "Bitte als normaler Benutzer starten (nicht mit sudo) – sudo wird bei Bedarf gefragt."
[ "$(uname -s)" = Linux ] || die "Nur für Linux."
case "$(uname -m)" in
  aarch64|arm64) ;;
  x86_64) warn "Das ist kein Raspberry Pi (x86_64) – geht trotzdem." ;;
  *) die "$(uname -m) wird nicht unterstützt. Bitte das 64-bit Raspberry Pi OS installieren." ;;
esac

say "Pakete installieren (git, Compiler, unzip)"
sudo apt-get update -qq
sudo apt-get install -y -qq git curl build-essential pkg-config unzip ca-certificates >/dev/null

if [ ! -x "$HOME/.cargo/bin/rustup" ]; then
  # Horizon braucht ein sehr neues Rust – das aus apt ist zu alt, darum rustup.
  say "Rust installieren"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
fi
# shellcheck disable=SC1091
. "$HOME/.cargo/env"
rustup update stable --no-self-update >/dev/null 2>&1 || true

say "Panel bauen (ein paar Minuten)"
cargo build --release --locked
sudo install -m 755 target/release/horizon-pi-panel /usr/local/bin/horizon-pi-panel

say "Datenordner $DATA einrichten"
mkdir -p "$DATA/scripts" "$DATA/servers" "$DATA/bin" "$DATA/game-files"
install -m 755 scripts/build-horizon.sh "$DATA/scripts/build-horizon.sh"

if ! grep -qs '^password_hash = "\$' "$DATA/panel.toml"; then
  say "Passwort für das Web-Panel festlegen (mindestens 8 Zeichen)"
  until horizon-pi-panel --data "$DATA" set-password; do warn "Nochmal bitte."; done
fi

# Ist der Panel-Port schon belegt (z. B. von einem anderen Dienst auf 8080)? Dann den nächsten freien nehmen.
sudo systemctl stop $SERVICE 2>/dev/null || true
port_busy() {
  if command -v ss >/dev/null; then ss -Hltn "sport = :$1" 2>/dev/null | grep -q .
  else (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; fi
}
BIND="$(sed -n 's/^bind = "\(.*\)"/\1/p' "$DATA/panel.toml")"
BIND="${BIND:-0.0.0.0:8080}"
PORT="${BIND##*:}"
if port_busy "$PORT"; then
  NEW=$PORT
  while port_busy "$NEW" || [ "$NEW" = 8130 ]; do NEW=$((NEW + 1)); done
  warn "Port $PORT ist schon belegt (von: $(sudo ss -Hltnp "sport = :$PORT" 2>/dev/null | grep -o 'users:(("[^"]*' | cut -d'"' -f2 | head -n1 || true)) – das Panel nimmt Port $NEW."
  if grep -q '^bind = ' "$DATA/panel.toml"; then
    sed -i "s/^bind = .*/bind = \"${BIND%:*}:$NEW\"/" "$DATA/panel.toml"
  else
    printf 'bind = "%s:%s"\n' "${BIND%:*}" "$NEW" >> "$DATA/panel.toml"
  fi
fi

say "Systemdienst einrichten (startet mit dem Pi)"
sudo tee /etc/systemd/system/$SERVICE.service >/dev/null <<EOF
[Unit]
Description=Horizon Pi Panel (Minecraft-Server-Verwaltung)
After=network-online.target
Wants=network-online.target

[Service]
User=$USER_NAME
Environment=HORIZON_PANEL_DIR=$DATA
Environment=PATH=$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin
WorkingDirectory=$DATA
ExecStart=/usr/local/bin/horizon-pi-panel serve
# Beim Stoppen speichert das Panel erst jede Welt und stoppt die Server sauber.
KillMode=mixed
TimeoutStopSec=260
Restart=on-failure
RestartSec=5
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
EOF
sudo systemctl daemon-reload
sudo systemctl enable $SERVICE >/dev/null 2>&1
sudo systemctl restart $SERVICE
sleep 2
systemctl is-active --quiet $SERVICE || { sudo journalctl -u $SERVICE -n 30 --no-pager; die "Der Dienst läuft nicht (Log oben)."; }

IP="$(hostname -I 2>/dev/null | awk '{ print $1 }')"
PORT="$(sed -n 's/^bind = "[^:]*:\([0-9]*\)"/\1/p' "$DATA/panel.toml")"
say "Fertig! Panel: http://${IP:-<pi-ip>}:${PORT:-8080}  (auch http://$(hostname).local:${PORT:-8080})"

if [ ! -x "$DATA/bin/horizon-server" ]; then
  echo
  echo "Als Nächstes muss Horizon selbst gebaut werden (für den Pi gibt es keinen fertigen Download)."
  echo "Das dauert auf dem Pi 5 grob 30–90 Minuten. Du kannst es jetzt hier oder später im Panel"
  echo "unter „Pi & Horizon“ → „Horizon bauen“ starten."
  read -r -p "Jetzt bauen? [j/N] " answer || answer=n
  case "$answer" in
    j|J|ja|y|Y) HORIZON_PANEL_DIR="$DATA" bash "$DATA/scripts/build-horizon.sh" ;;
  esac
fi

echo
echo "Danach: Spieldateien am PC vorbereiten und im Panel hochladen – siehe README.md."
