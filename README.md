# Horizon Pi Panel

Ein eigenes Web-Panel für den **Raspberry Pi 5**, mit dem du [Horizon](https://github.com/horizon-rust/horizon)-Server
(Minecraft Java 26.3, in Rust neu geschrieben) hostest und vom Handy aus verwaltest.
Ein einziges Rust-Programm, keine Datenbank, kein Docker, kein Java.

- **Mehrere Server** nebeneinander, jeder mit eigener Welt und eigenem Port
- **Start / Stop / Neustart**, Autostart beim Hochfahren, Neustart nach Abstürzen
- **Live-Konsole** mit Befehlseingabe, Verlauf und Schnellbefehlen
- **Backups**: jetzt sichern, automatisch (z. B. alle 6 h), wiederherstellen, herunterladen, löschen
- **Pi-Status**: CPU, Temperatur, RAM, Speicherplatz
- **Einstellungen** als Formular oder direkt als `server.toml`
- **Horizon bauen/aktualisieren** und **Spieldateien hochladen** direkt im Panel
- Fürs Handy gemacht, Passwortschutz

> Horizon ist ein inoffizielles Projekt und nicht von Mojang oder Microsoft. Jeder Spieler und der Server
> brauchen eine eigene, gekaufte Minecraft: Java Edition. Dieses Panel lädt nichts von Mojang herunter.

## Was du brauchst

- Raspberry Pi 5 (8 GB passt super) mit **Raspberry Pi OS 64-bit** (Bookworm oder neuer)
- Am besten eine SSD oder schnelle SD-Karte, **mindestens 16 GB frei** (der Horizon-Build braucht ~12 GB)
- Einen Windows-PC mit Minecraft Java 26.3 (einmal gestartet) und Java 25 – für die Spieldateien
- Kühlung für den Pi (der Build und Minecraft machen ihn warm)

## Installation

Per SSH auf dem Pi:

```bash
git clone https://github.com/Lutziano888/Minecraft-Rust-Server-Panel.git
cd Minecraft-Rust-Server-Panel
./install.sh
```

Das Skript installiert Rust, baut das Panel, fragt nach einem **Passwort** und richtet den Dienst
`horizon-pi-panel` ein. Danach öffnest du am Handy (im selben WLAN):

```
http://<IP-des-Pi>:5341        oder        http://<name-des-pi>.local:5341
```

## Erste Schritte

1. **Horizon bauen** – für den Pi gibt es keine fertigen Downloads, darum wird Horizon aus dem Quellcode gebaut.
   Im Panel unter **Pi & Horizon → Horizon bauen** (oder direkt am Ende von `install.sh`).
   Dauert auf dem Pi 5 grob 30–90 Minuten. Du kannst die Seite währenddessen schließen.
   **Schneller:** am PC bauen und rüberkopieren, siehe [Horizon am PC bauen](#horizon-am-pc-bauen-schneller).
2. **Spieldateien vorbereiten** (am Windows-PC, einmalig und nach Minecraft-Updates):
   ```powershell
   horizon-prepare --prepare --server-jar C:\Pfad\zu\server.jar
   ```
   Die `server.jar` von 26.3 findest du in `versions/26.3/26.3.json` deines Minecraft-Launchers
   (`downloads.server.url`). Den fertigen Ordner `26.3` (meist
   `%LOCALAPPDATA%\Horizon\game-files\horizon-client\26.3`) per Rechtsklick → **ZIP-komprimierter Ordner** packen.
3. **Hochladen**: Panel → **Pi & Horizon → Spieldateien hochladen**.
4. **Server anlegen**: Tab **Neu**, Namen eingeben, EULA bestätigen, fertig. Dann **▶ Start**.
5. **Verbinden**: In Minecraft/Horizon als Serveradresse `<IP-des-Pi>:25565` eintragen
   (die genaue Adresse steht im Server unter **Übersicht**).

### Horizon am PC bauen (schneller)

Ein Desktop-PC baut Horizon in einem Bruchteil der Zeit. **Unter Linux (z. B. Arch)** direkt im Terminal:

```bash
git clone https://github.com/Lutziano888/Minecraft-Rust-Server-Panel.git
cd Minecraft-Rust-Server-Panel
./scripts/cross-build-horizon.sh --pi BENUTZER@raspberrypi.local
```

**Unter Windows** geht das mit WSL (Linux in Windows):

1. PowerShell **als Administrator**: `wsl --install -d Ubuntu`, neu starten, Benutzer anlegen.
2. Im Ubuntu-Fenster:
   ```bash
   git clone https://github.com/Lutziano888/Minecraft-Rust-Server-Panel.git
   cd Minecraft-Rust-Server-Panel
   ./scripts/cross-build-horizon.sh --pi BENUTZER@raspberrypi.local
   ```
   `BENUTZER` ist dein Benutzername auf dem Pi, statt `raspberrypi.local` geht auch die IP.

Das Skript installiert Rust und Zig, baut Horizon für ARM64 (passend zur glibc von Raspberry Pi OS) und kopiert
`horizon-server` und `horizon-engine` per SSH nach `~/horizon-panel/bin/` auf den Pi. Es fragt dabei nach dem
Pi-Passwort. Ohne `--pi` landen die Programme nur in `~/horizon-pi-bin/`.
Für Updates einfach dasselbe nochmal ausführen und danach die Server im Panel neu starten.

### Online-Modus

- **An** (Standard): Jeder spielt mit seinem eigenen Minecraft-Konto (Horizon-Spieler mit dem Microsoft-Konto aus
  dem Horizon-Launcher). Sicher, auch übers Internet.
- **Aus**: Keine Kontoprüfung – jeder kann sich unter jedem Namen einloggen. Nur im Heimnetz oder über
  Tailscale benutzen und die Whitelist anlassen.

## Von unterwegs spielen und verwalten

Am einfachsten und sichersten mit **[Tailscale](https://tailscale.com)**: auf dem Pi
`curl -fsSL https://tailscale.com/install.sh | sh && sudo tailscale up`, auf Handy/PC die App installieren.
Dann erreichst du Panel und Server über die Tailscale-Adresse des Pi (`100.x.y.z`), ohne etwas am Router
freizugeben.

**Das Panel nie per Portweiterleitung ins Internet stellen.** Es läuft über einfaches HTTP. Wenn Freunde ohne
Tailscale joinen sollen, gib am Router nur den **Minecraft-Port** (25565) frei und lass den Online-Modus an.

## Wo liegt was?

```
~/horizon-panel/
├── panel.toml              Panel-Einstellungen (Port, Passwort-Hash)
├── bin/                    horizon-server und horizon-engine (gebaut)
├── game-files/26.3/        die hochgeladenen Spieldateien
├── horizon-src/            Horizon-Quellcode (für Updates)
├── scripts/build-horizon.sh
└── servers/<name>/         ein Ordner pro Server: server.toml, world/, backups/, logs/
```

Jeder Serverordner ist ein ganz normaler Horizon-Serverordner – du kannst ihn auch ohne Panel mit
`~/horizon-panel/bin/horizon-server --dir ~/horizon-panel/servers/<name> …` benutzen.

## Nützliche Befehle

```bash
sudo systemctl status horizon-pi-panel     # läuft das Panel?
sudo journalctl -u horizon-pi-panel -f     # Panel-Log live
sudo systemctl restart horizon-pi-panel    # neu starten (stoppt alle Server sauber, Autostart-Server starten wieder)
horizon-pi-panel set-password              # Passwort vergessen? Neu setzen, dann Dienst neu starten
horizon-pi-panel paths                     # alle Pfade anzeigen
```

**Panel aktualisieren:** im Ordner `Minecraft-Rust-Server-Panel` → `git pull && ./install.sh`.

**Anderer Port fürs Panel:** in `~/horizon-panel/panel.toml` z. B. `bind = "0.0.0.0:8081"`, dann Dienst neu starten.

## Tipps für den Pi

- Sichtweite 6–10 und Simulationsweite 6–8 halten den Pi entspannt.
- Über 80 °C drosselt der Pi – aktiver Kühler oder Gehäuse mit Lüfter hilft.
- Jeder laufende Server braucht etwa 0,5–1 GB RAM. Mit 8 GB passen mehrere locker.
- Automatische Backups landen im Serverordner. Lad ab und zu eins herunter, falls die SD-Karte stirbt.

## Deinstallieren

```bash
sudo systemctl disable --now horizon-pi-panel
sudo rm /etc/systemd/system/horizon-pi-panel.service /usr/local/bin/horizon-pi-panel
# Welten und Backups liegen weiter in ~/horizon-panel – erst löschen, wenn du sie nicht mehr brauchst.
```

## Technik

Das Panel (Rust, axum) startet für jeden Server `horizon-server run` als Kindprozess, liest dessen Ausgabe für
die Live-Konsole und spricht mit der Control-API des Servers (`.horizon/api.json`). Backups, Wiederherstellen und
Einstellungen laufen über das offizielle `horizon-server`-CLI, damit alles so funktioniert, wie Horizon es vorsieht.
Das eingebaute Panel jedes Servers wird abgeschaltet, damit sich mehrere Server nicht um Port 8130 streiten.

Lizenz: MIT
