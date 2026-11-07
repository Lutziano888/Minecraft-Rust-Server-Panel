//! Die Server: Ordner unter <data>/servers/<id>, jeder mit seinem eigenen `horizon-server run`
//! als Kindprozess des Panels. Das Panel liest seine Ausgabe (Live-Konsole), fragt seine
//! Control-API nach dem Status und startet ihn nach einem Absturz neu.

use crate::{config::Paths, horizon};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, Command},
    sync::broadcast,
};

const LOG_LINES: usize = 2000;
const META: &str = ".panel.json";
const STOP_WAIT: Duration = Duration::from_secs(200);
const API_TIMEOUT: Duration = Duration::from_secs(8);

/// Einstellungen, die die Oberfläche als Formular zeigt (Schlüssel wie in server.toml).
pub const FORM_KEYS: &[&str] = &[
    "server.motd",
    "server.port",
    "server.max_players",
    "server.online_mode",
    "server.whitelist",
    "server.view_distance",
    "server.simulation_distance",
    "world.game_mode",
    "world.difficulty",
    "world.hardcore",
    "world.seed",
    "engine.low_memory",
    "engine.restart_on_crash",
    "backups.every",
    "backups.keep",
];

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Meta {
    #[serde(default)]
    pub autostart: bool,
    #[serde(default)]
    pub created: u64,
}

#[derive(Clone, Serialize)]
pub struct LogLine {
    pub n: u64,
    pub text: String,
}

#[derive(Default)]
struct LogBuf {
    lines: VecDeque<LogLine>,
}

pub struct Server {
    pub id: String,
    pub dir: PathBuf,
    program: PathBuf,
    meta: Mutex<Meta>,
    pid: Mutex<Option<u32>>,
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    log: Mutex<LogBuf>,
    next_line: AtomicU64,
    tx: broadcast::Sender<LogLine>,
    wanted: AtomicBool,
    stopping: AtomicBool,
    status: Mutex<Value>,
    crashes: Mutex<Vec<Instant>>,
    started_at: Mutex<Option<Instant>>,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub fn valid_id(id: &str) -> bool {
    let b = id.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

impl Server {
    fn new(id: String, dir: PathBuf, program: PathBuf) -> Arc<Self> {
        let meta = std::fs::read_to_string(dir.join(META))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let (tx, _) = broadcast::channel(512);
        let server = Arc::new(Self {
            id,
            dir,
            program,
            meta: Mutex::new(meta),
            pid: Mutex::new(None),
            stdin: tokio::sync::Mutex::new(None),
            log: Mutex::new(LogBuf::default()),
            next_line: AtomicU64::new(1),
            tx,
            wanted: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            status: Mutex::new(Value::Null),
            crashes: Mutex::new(Vec::new()),
            started_at: Mutex::new(None),
        });
        // Das Ende des letzten Logs, damit die Konsole nicht leer ist.
        if let Ok(text) = std::fs::read_to_string(server.dir.join("logs/latest.log")) {
            let lines: Vec<&str> = text.lines().collect();
            for l in &lines[lines.len().saturating_sub(150)..] {
                server.push(l.to_string());
            }
            server.push("[Panel] ─── Ende des letzten Logs ───".into());
        }
        server
    }

    pub fn meta(&self) -> Meta {
        self.meta.lock().unwrap().clone()
    }

    pub fn set_autostart(&self, on: bool) -> Result<(), String> {
        let mut m = self.meta.lock().unwrap();
        m.autostart = on;
        write_meta(&self.dir, &m)
    }

    fn push(&self, text: String) {
        let n = self.next_line.fetch_add(1, Ordering::SeqCst);
        let line = LogLine { n, text };
        {
            let mut log = self.log.lock().unwrap();
            log.lines.push_back(line.clone());
            while log.lines.len() > LOG_LINES {
                log.lines.pop_front();
            }
        }
        let _ = self.tx.send(line);
    }

    pub fn note(&self, text: &str) {
        self.push(format!("[Panel] {text}"));
    }

    pub fn backlog(&self) -> Vec<LogLine> {
        self.log.lock().unwrap().lines.iter().cloned().collect()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<LogLine> {
        self.tx.subscribe()
    }

    pub fn alive(&self) -> bool {
        self.pid.lock().unwrap().is_some()
    }

    pub fn port(&self) -> Option<u16> {
        read_toml(&self.dir)
            .ok()
            .and_then(|d| d.get("server")?.get("port")?.as_integer())
            .and_then(|p| u16::try_from(p).ok())
    }

    /// Der Zustand, wie die Oberfläche ihn zeigt.
    pub fn info(&self) -> Value {
        let parsed = read_toml(&self.dir);
        let config_error = parsed.as_ref().err().cloned();
        let doc = parsed.ok();
        let get = |section: &str, key: &str| {
            doc.as_ref()
                .and_then(|d| d.get(section)?.get(key).map(item_json))
                .unwrap_or(Value::Null)
        };
        let status = self.status.lock().unwrap().clone();
        let alive = self.alive();
        let phase = if self.stopping.load(Ordering::SeqCst) && alive {
            "stopping".to_string()
        } else if !alive {
            "offline".to_string()
        } else {
            status["phase"].as_str().unwrap_or("booting").to_string()
        };
        let game = &status["sample"]["game"];
        let names: Vec<Value> = game["players"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|p| p["name"].as_str().map(|n| Value::String(n.into())))
                    .collect()
            })
            .unwrap_or_default();
        json!({
            "id": self.id,
            "phase": phase,
            "process": alive,
            "port": get("server", "port"),
            "motd": get("server", "motd"),
            "max_players": get("server", "max_players"),
            "game_mode": get("world", "game_mode"),
            "autostart": self.meta().autostart,
            "players": if alive { Value::from(names.len()) } else { Value::Null },
            "player_names": names,
            "tps": game["tps"],
            "mspt": game["mspt"]["average"],
            "rss_bytes": status["sample"]["rss_bytes"],
            "cpu_percent": status["sample"]["cpu_percent"],
            "uptime_seconds": if alive {
                status["engine_uptime_seconds"].as_u64().map(Value::from).unwrap_or_else(|| {
                    self.started_at.lock().unwrap().map(|t| Value::from(t.elapsed().as_secs())).unwrap_or(Value::Null)
                })
            } else { Value::Null },
            "crashes": status["crashes"],
            "last_crash": status["last_crash"],
            "version": status["version"],
            "config_error": config_error,
        })
    }

    async fn refresh(&self) {
        if !self.alive() {
            *self.status.lock().unwrap() = Value::Null;
            return;
        }
        match horizon::api(&self.dir, "GET", "/api/v1/status", None, API_TIMEOUT).await {
            Ok(v) => *self.status.lock().unwrap() = v,
            Err(_) => *self.status.lock().unwrap() = Value::Null,
        }
    }

    fn api_phase(&self) -> Option<String> {
        self.status.lock().unwrap()["phase"]
            .as_str()
            .map(str::to_owned)
    }

    /// Server starten: einen neuen Supervisor, oder im laufenden Supervisor das Spiel wieder an.
    pub async fn start(self: &Arc<Self>, paths: &Paths) -> Result<String, String> {
        if self.alive() {
            if self.stopping.load(Ordering::SeqCst) {
                return Err("Der Server fährt gerade herunter – kurz warten.".into());
            }
            self.refresh().await;
            if self.api_phase().as_deref() == Some("stopped") {
                horizon::api(&self.dir, "POST", "/api/v1/start", Some(&json!({})), API_TIMEOUT)
                    .await?;
                self.note("Spiel wird wieder gestartet");
                return Ok("Spiel startet".into());
            }
            return Ok("Läuft schon".into());
        }
        check_ready(paths)?;
        self.wanted.store(true, Ordering::SeqCst);
        self.spawn()?;
        Ok("Server startet".into())
    }

    fn spawn(self: &Arc<Self>) -> Result<(), String> {
        let mut cmd = Command::new(&self.program);
        cmd.arg("--dir")
            .arg(&self.dir)
            .arg("run")
            .current_dir(&self.dir)
            .env("HORIZON_NO_UPDATE_CHECK", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("{} konnte nicht starten: {e}", self.program.display()))?;
        let pid = child.id();
        *self.pid.lock().unwrap() = pid;
        *self.started_at.lock().unwrap() = Some(Instant::now());
        self.stopping.store(false, Ordering::SeqCst);
        let stdin = child.stdin.take();
        let out = child.stdout.take();
        let err = child.stderr.take();
        self.note(&format!("Starte horizon-server (PID {})", pid.unwrap_or(0)));
        let me = self.clone();
        tokio::spawn(async move {
            *me.stdin.lock().await = stdin;
            let a = {
                let me = me.clone();
                tokio::spawn(async move {
                    if let Some(out) = out {
                        let mut lines = BufReader::new(out).lines();
                        while let Ok(Some(l)) = lines.next_line().await {
                            me.push(l);
                        }
                    }
                })
            };
            let b = {
                let me = me.clone();
                tokio::spawn(async move {
                    if let Some(err) = err {
                        let mut lines = BufReader::new(err).lines();
                        while let Ok(Some(l)) = lines.next_line().await {
                            me.push(l);
                        }
                    }
                })
            };
            let status = child.wait().await;
            let _ = a.await;
            let _ = b.await;
            *me.pid.lock().unwrap() = None;
            *me.stdin.lock().await = None;
            *me.status.lock().unwrap() = Value::Null;
            *me.started_at.lock().unwrap() = None;
            let asked = me.stopping.swap(false, Ordering::SeqCst) || !me.wanted.load(Ordering::SeqCst);
            let ok = status.as_ref().is_ok_and(|s| s.success());
            match &status {
                Ok(s) => me.note(&format!("horizon-server beendet ({s})")),
                Err(e) => me.note(&format!("horizon-server beendet: {e}")),
            }
            if asked || ok {
                me.wanted.store(false, Ordering::SeqCst);
                return;
            }
            // Unerwartet beendet: höchstens 3 Neustarts in 10 Minuten.
            let retry = {
                let mut c = me.crashes.lock().unwrap();
                let now = Instant::now();
                c.retain(|t| now.duration_since(*t) < Duration::from_secs(600));
                c.push(now);
                c.len() <= 3
            };
            if !retry {
                me.wanted.store(false, Ordering::SeqCst);
                me.note("Zu viele Abstürze in 10 Minuten – kein automatischer Neustart. Log oben prüfen.");
                return;
            }
            me.note("Unerwartet beendet – Neustart in 10 Sekunden");
            tokio::time::sleep(Duration::from_secs(10)).await;
            if me.wanted.load(Ordering::SeqCst) && !me.alive() {
                if let Err(e) = me.spawn() {
                    me.note(&e);
                }
            }
        });
        Ok(())
    }

    /// Sauber stoppen: die API bitten (speichert die Welt), sonst SIGTERM; nach 200 s SIGKILL.
    pub async fn stop(self: &Arc<Self>) -> Result<String, String> {
        self.wanted.store(false, Ordering::SeqCst);
        if !self.alive() {
            return Ok("Ist schon aus".into());
        }
        self.stopping.store(true, Ordering::SeqCst);
        self.note("Server wird gestoppt (Welt wird gespeichert)…");
        if horizon::api(&self.dir, "POST", "/api/v1/stop", Some(&json!({})), API_TIMEOUT)
            .await
            .is_err()
        {
            self.signal(libc::SIGTERM);
        }
        let me = self.clone();
        tokio::spawn(async move {
            let deadline = Instant::now() + STOP_WAIT;
            while me.alive() && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            if me.alive() {
                me.note("Reagiert nicht – wird hart beendet");
                me.signal(libc::SIGKILL);
            }
        });
        Ok("Wird gestoppt".into())
    }

    /// Stoppen und warten, bis der Prozess weg ist.
    pub async fn stop_and_wait(self: &Arc<Self>) {
        let _ = self.stop().await;
        let deadline = Instant::now() + STOP_WAIT + Duration::from_secs(5);
        while self.alive() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    fn signal(&self, sig: i32) {
        if let Some(pid) = *self.pid.lock().unwrap() {
            unsafe {
                libc::kill(pid as i32, sig);
            }
        }
    }

    pub async fn restart(self: &Arc<Self>, paths: &Paths) -> Result<String, String> {
        if self.alive() && !self.stopping.load(Ordering::SeqCst) {
            self.refresh().await;
            if self.api_phase().as_deref() == Some("running") {
                horizon::api(&self.dir, "POST", "/api/v1/restart", Some(&json!({})), API_TIMEOUT)
                    .await?;
                self.note("Spiel wird neu gestartet");
                return Ok("Startet neu".into());
            }
        }
        if self.alive() {
            self.stop_and_wait().await;
        }
        self.start(paths).await
    }

    /// Ein Konsolenbefehl: über die API (mit Antwort), sonst direkt auf stdin.
    pub async fn command(&self, line: &str) -> Result<Vec<String>, String> {
        let line = line.trim().trim_start_matches('/').to_string();
        if line.is_empty() || line.contains('\n') {
            return Err("Leerer Befehl".into());
        }
        if !self.alive() {
            return Err("Der Server ist aus.".into());
        }
        self.push(format!("> {line}"));
        match horizon::api(
            &self.dir,
            "POST",
            "/api/v1/command",
            Some(&json!({ "line": line })),
            Duration::from_secs(30),
        )
        .await
        {
            Ok(reply) => {
                let lines = horizon::reply_lines(&reply);
                for l in &lines {
                    self.push(l.clone());
                }
                Ok(lines)
            }
            Err(api_err) => {
                let mut stdin = self.stdin.lock().await;
                match stdin.as_mut() {
                    Some(s) => {
                        s.write_all(format!("{line}\n").as_bytes())
                            .await
                            .map_err(|e| e.to_string())?;
                        s.flush().await.map_err(|e| e.to_string())?;
                        Ok(vec![])
                    }
                    None => Err(api_err),
                }
            }
        }
    }

    pub fn settings(&self) -> Result<Value, String> {
        let raw = std::fs::read_to_string(self.dir.join("server.toml"))
            .map_err(|e| format!("server.toml: {e}"))?;
        let doc: toml_edit::DocumentMut = raw.parse().map_err(|e| format!("server.toml: {e}"))?;
        let mut values = Map::new();
        for key in FORM_KEYS {
            let (section, field) = key.split_once('.').unwrap();
            let v = doc
                .get(section)
                .and_then(|s| s.get(field))
                .map(item_json)
                .unwrap_or(Value::Null);
            values.insert((*key).into(), v);
        }
        Ok(json!({ "values": values, "raw": raw, "autostart": self.meta().autostart }))
    }

    pub fn backups(&self) -> Vec<Value> {
        let mut out: Vec<(u64, Value)> = std::fs::read_dir(self.dir.join("backups"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if !name.ends_with(".zip") {
                    return None;
                }
                let md = e.metadata().ok()?;
                let modified = md
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs());
                Some((
                    modified,
                    json!({
                        "name": name,
                        "bytes": md.len(),
                        "modified": modified,
                        "auto": name.ends_with("-auto.zip"),
                    }),
                ))
            })
            .collect();
        out.sort_by_key(|e| std::cmp::Reverse(e.0));
        out.into_iter().map(|(_, v)| v).collect()
    }

    pub fn backup_path(&self, name: &str) -> Result<PathBuf, String> {
        if name.is_empty()
            || name.contains('/')
            || name.contains('\\')
            || name.contains("..")
            || !name.ends_with(".zip")
        {
            return Err("Ungültiger Backup-Name".into());
        }
        let p = self.dir.join("backups").join(name);
        if p.is_file() {
            Ok(p)
        } else {
            Err("Backup nicht gefunden".into())
        }
    }
}

fn item_json(item: &toml_edit::Item) -> Value {
    if let Some(b) = item.as_bool() {
        Value::Bool(b)
    } else if let Some(i) = item.as_integer() {
        Value::from(i)
    } else if let Some(f) = item.as_float() {
        Value::from(f)
    } else if let Some(s) = item.as_str() {
        Value::String(s.into())
    } else {
        Value::String(item.to_string().trim().to_owned())
    }
}

fn read_toml(dir: &Path) -> Result<toml_edit::DocumentMut, String> {
    std::fs::read_to_string(dir.join("server.toml"))
        .map_err(|e| e.to_string())?
        .parse()
        .map_err(|e: toml_edit::TomlError| e.to_string())
}

fn write_meta(dir: &Path, meta: &Meta) -> Result<(), String> {
    let text = serde_json::to_string_pretty(meta).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(META), text).map_err(|e| e.to_string())
}

/// Sind Programm, Spiel und Spieldateien da?
pub fn check_ready(paths: &Paths) -> Result<(), String> {
    if !paths.horizon_server.is_file() {
        return Err(format!(
            "horizon-server fehlt ({}). Unter „Pi & Horizon“ auf „Horizon bauen“ tippen.",
            paths.horizon_server.display()
        ));
    }
    if !paths.horizon_engine.is_file() {
        return Err(format!(
            "Das Spielprogramm fehlt ({}). Unter „Pi & Horizon“ auf „Horizon bauen“ tippen.",
            paths.horizon_engine.display()
        ));
    }
    if !paths.game_files.join("horizon-resources.json").is_file() {
        return Err(
            "Die Spieldateien fehlen. Am PC mit horizon-prepare vorbereiten und unter „Pi & Horizon“ als ZIP hochladen."
                .into(),
        );
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct CreateRequest {
    pub id: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub motd: String,
    #[serde(default)]
    pub seed: String,
    #[serde(default = "survival")]
    pub game_mode: String,
    #[serde(default = "easy")]
    pub difficulty: String,
    #[serde(default)]
    pub max_players: Option<u32>,
    #[serde(default)]
    pub whitelist: bool,
    #[serde(default = "yes")]
    pub online_mode: bool,
    #[serde(default)]
    pub op: String,
    #[serde(default)]
    pub eula: bool,
    #[serde(default = "yes")]
    pub autostart: bool,
}
fn survival() -> String {
    "survival".into()
}
fn easy() -> String {
    "easy".into()
}
fn yes() -> bool {
    true
}

pub fn valid_player(name: &str) -> bool {
    (1..=16).contains(&name.len()) && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

pub struct Manager {
    pub paths: Paths,
    first_port: u16,
    servers: Mutex<BTreeMap<String, Arc<Server>>>,
}

impl Manager {
    pub fn load(paths: Paths, first_port: u16) -> Arc<Self> {
        let _ = std::fs::create_dir_all(&paths.servers);
        let mut map = BTreeMap::new();
        if let Ok(entries) = std::fs::read_dir(&paths.servers) {
            for e in entries.flatten() {
                let id = e.file_name().to_string_lossy().into_owned();
                if valid_id(&id) && e.path().join("server.toml").is_file() {
                    map.insert(
                        id.clone(),
                        Server::new(id, e.path(), paths.horizon_server.clone()),
                    );
                }
            }
        }
        Arc::new(Self {
            paths,
            first_port,
            servers: Mutex::new(map),
        })
    }

    pub fn get(&self, id: &str) -> Option<Arc<Server>> {
        self.servers.lock().unwrap().get(id).cloned()
    }

    pub fn all(&self) -> Vec<Arc<Server>> {
        self.servers.lock().unwrap().values().cloned().collect()
    }

    /// Status aller laufenden Server alle 2 Sekunden auffrischen.
    pub fn spawn_poller(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                for s in me.all() {
                    s.refresh().await;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });
    }

    pub async fn autostart(self: &Arc<Self>) {
        for s in self.all() {
            if s.meta().autostart {
                match s.start(&self.paths).await {
                    Ok(_) => {}
                    Err(e) => s.note(&format!("Autostart nicht möglich: {e}")),
                }
            }
        }
    }

    pub async fn stop_all(&self) {
        let all = self.all();
        let waits = all.iter().map(|s| {
            let s = s.clone();
            async move { s.stop_and_wait().await }
        });
        futures_util::future::join_all(waits).await;
    }

    fn used_ports(&self, except: Option<&str>) -> Vec<u16> {
        self.all()
            .iter()
            .filter(|s| Some(s.id.as_str()) != except)
            .filter_map(|s| s.port())
            .collect()
    }

    pub fn check_port(&self, port: u16, except: Option<&str>) -> Result<(), String> {
        if port < 1024 {
            return Err("Port muss 1024 oder höher sein".into());
        }
        if port == 8080 || port == 8130 {
            return Err(format!("Port {port} ist für Panels reserviert"));
        }
        if self.used_ports(except).contains(&port) {
            return Err(format!("Port {port} benutzt schon ein anderer Server"));
        }
        Ok(())
    }

    fn free_port(&self) -> u16 {
        let used = self.used_ports(None);
        (self.first_port..=u16::MAX)
            .find(|p| !used.contains(p) && *p != 8080 && *p != 8130)
            .unwrap_or(25565)
    }

    pub async fn create(self: &Arc<Self>, req: CreateRequest) -> Result<Arc<Server>, String> {
        let id = req.id.trim().to_lowercase();
        if !valid_id(&id) {
            return Err("Name: nur Kleinbuchstaben, Ziffern und -, höchstens 32 Zeichen".into());
        }
        if !req.eula {
            return Err("Ohne Zustimmung zur Minecraft-EULA kann kein Server laufen.".into());
        }
        if self.get(&id).is_some() {
            return Err(format!("Server „{id}“ gibt es schon"));
        }
        if !["survival", "creative", "adventure", "spectator"].contains(&req.game_mode.as_str()) {
            return Err("Unbekannter Spielmodus".into());
        }
        if !["peaceful", "easy", "normal", "hard"].contains(&req.difficulty.as_str()) {
            return Err("Unbekannter Schwierigkeitsgrad".into());
        }
        let op = req.op.trim().to_string();
        if !op.is_empty() && !valid_player(&op) {
            return Err("Spielername: 1–16 Zeichen, nur Buchstaben, Ziffern und _".into());
        }
        if !self.paths.horizon_server.is_file() {
            return Err("horizon-server fehlt noch – erst unter „Pi & Horizon“ Horizon bauen.".into());
        }
        let port = req.port.unwrap_or_else(|| self.free_port());
        self.check_port(port, None)?;
        let dir = self.paths.servers.join(&id);
        if dir.exists() {
            return Err(format!("Der Ordner {} existiert schon", dir.display()));
        }
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

        let port_text = port.to_string();
        let mut args: Vec<&str> = vec!["init", "--yes", "--eula", "--port", &port_text];
        let seed = req.seed.trim().to_string();
        if !seed.is_empty() {
            args.extend(["--seed", &seed]);
        }
        args.push(if req.whitelist { "--whitelist" } else { "--no-whitelist" });
        if !op.is_empty() {
            args.extend(["--op", &op, "--player", &op]);
        }
        if !req.online_mode {
            args.push("--offline");
        }
        if let Err(e) = horizon::cli(
            &self.paths.horizon_server,
            &dir,
            &args,
            Duration::from_secs(120),
        )
        .await
        {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(format!("horizon-server init: {e}"));
        }

        let motd = if req.motd.trim().is_empty() {
            format!("{id} – Horizon auf dem Pi")
        } else {
            req.motd.trim().to_string()
        };
        let result = (|| -> Result<(), String> {
            let mut doc = read_toml(&dir)?;
            let set = |doc: &mut toml_edit::DocumentMut, s: &str, k: &str, v: toml_edit::Value| {
                if doc.get(s).is_none() {
                    doc[s] = toml_edit::table();
                }
                doc[s][k] = toml_edit::value(v);
            };
            // Das eingebaute Panel jedes Servers aus: dieses Panel ersetzt es (sonst Streit um Port 8130).
            set(&mut doc, "panel", "bind", "".into());
            set(
                &mut doc,
                "engine",
                "path",
                abs(&self.paths.horizon_engine).into(),
            );
            set(
                &mut doc,
                "engine",
                "resources",
                abs(&self.paths.game_files).into(),
            );
            set(&mut doc, "server", "motd", motd.as_str().into());
            if let Some(m) = req.max_players {
                set(&mut doc, "server", "max_players", (m.clamp(1, 1000) as i64).into());
            }
            set(&mut doc, "world", "game_mode", req.game_mode.as_str().into());
            set(&mut doc, "world", "difficulty", req.difficulty.as_str().into());
            std::fs::write(dir.join("server.toml"), doc.to_string()).map_err(|e| e.to_string())?;
            write_meta(
                &dir,
                &Meta {
                    autostart: req.autostart,
                    created: now_secs(),
                },
            )
        })();
        if let Err(e) = result {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
        let server = Server::new(id.clone(), dir, self.paths.horizon_server.clone());
        server.note(&format!("Server „{id}“ angelegt (Port {port})"));
        self.servers.lock().unwrap().insert(id, server.clone());
        Ok(server)
    }

    pub async fn delete(&self, id: &str) -> Result<(), String> {
        let s = self.get(id).ok_or("Server nicht gefunden")?;
        if s.alive() {
            return Err("Erst den Server stoppen.".into());
        }
        let dir = s.dir.clone();
        tokio::task::spawn_blocking(move || std::fs::remove_dir_all(dir))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        self.servers.lock().unwrap().remove(id);
        Ok(())
    }

    /// Formularwerte über `horizon-server config set` speichern (prüft und übernimmt live, was geht).
    pub async fn set_settings(&self, s: &Arc<Server>, changes: &Map<String, Value>) -> Result<String, String> {
        let mut args: Vec<String> = vec!["config".into(), "set".into()];
        for (key, value) in changes {
            if !FORM_KEYS.contains(&key.as_str()) {
                return Err(format!("{key} kann hier nicht geändert werden"));
            }
            let text = match value {
                Value::String(t) => t.clone(),
                Value::Bool(b) => b.to_string(),
                Value::Number(n) => n.to_string(),
                _ => return Err(format!("{key}: ungültiger Wert")),
            };
            if text.contains('\n') {
                return Err(format!("{key}: kein Zeilenumbruch erlaubt"));
            }
            if key == "server.port" {
                let port: u16 = text.parse().map_err(|_| "Port: Zahl von 1024 bis 65535")?;
                self.check_port(port, Some(&s.id))?;
            }
            args.push(format!("{key}={text}"));
        }
        if args.len() == 2 {
            return Ok("Nichts geändert".into());
        }
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = horizon::cli(&self.paths.horizon_server, &s.dir, &refs, Duration::from_secs(30)).await?;
        s.note("Einstellungen gespeichert");
        Ok(out)
    }

    pub fn set_raw(&self, s: &Arc<Server>, text: &str) -> Result<(), String> {
        let doc: toml_edit::DocumentMut = text
            .parse()
            .map_err(|e: toml_edit::TomlError| format!("TOML-Fehler: {e}"))?;
        if let Some(port) = doc
            .get("server")
            .and_then(|t| t.get("port"))
            .and_then(|p| p.as_integer())
        {
            let port = u16::try_from(port).map_err(|_| "Port außerhalb von 1–65535")?;
            self.check_port(port, Some(&s.id))?;
        }
        let path = s.dir.join("server.toml");
        let tmp = s.dir.join("server.toml.panel-tmp");
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        s.note("server.toml von Hand gespeichert – gilt ab dem nächsten Start");
        Ok(())
    }
}

fn abs(p: &Path) -> String {
    std::path::absolute(p)
        .unwrap_or_else(|_| p.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        assert!(valid_id("survival"));
        assert!(valid_id("welt-2"));
        assert!(!valid_id("-x"));
        assert!(!valid_id("Groß"));
        assert!(!valid_id("../etc"));
        assert!(!valid_id(""));
    }

    #[test]
    fn players() {
        assert!(valid_player("Lutz_88"));
        assert!(!valid_player("a b"));
        assert!(!valid_player(""));
    }
}
