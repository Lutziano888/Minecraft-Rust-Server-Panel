//! Die HTTP-Schnittstelle des Panels und die eingebettete Oberfläche.

use crate::{
    auth::{self, Auth},
    config::PanelConfig,
    horizon,
    jobs::{self, Jobs},
    servers::{CreateRequest, Manager},
    system::Sampler,
};
use axum::{
    body::Body,
    extract::{ConnectInfo, DefaultBodyLimit, Path, Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{delete, get, post, put},
    Json, Router,
};
use futures_util::StreamExt as _;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::{
    convert::Infallible,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::AsyncWriteExt;
use tokio_stream::wrappers::{errors::BroadcastStreamRecvError, BroadcastStream};

pub struct App {
    pub mgr: Arc<Manager>,
    pub auth: Auth,
    pub jobs: Arc<Jobs>,
    pub sampler: Arc<Sampler>,
    pub cfg: Mutex<PanelConfig>,
    pub data: PathBuf,
    pub horizon_version: Mutex<Option<String>>,
}

type St = State<Arc<App>>;

const INDEX: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");
const APP_CSS: &str = include_str!("../web/app.css");
const ICON: &str = include_str!("../web/icon.svg");

fn err(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, Json(json!({ "error": msg.into() }))).into_response()
}
fn bad(msg: impl Into<String>) -> Response {
    err(StatusCode::BAD_REQUEST, msg)
}
fn ok(v: Value) -> Response {
    Json(v).into_response()
}
fn result(r: Result<String, String>) -> Response {
    match r {
        Ok(m) => ok(json!({ "ok": true, "message": m })),
        Err(e) => err(StatusCode::CONFLICT, e),
    }
}

pub fn router(app: Arc<App>) -> Router {
    let api = Router::new()
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/me", get(|| async { ok(json!({ "ok": true })) }))
        .route("/api/system", get(system))
        .route("/api/servers", get(list).post(create))
        .route("/api/servers/{id}", get(one).delete(remove))
        .route("/api/servers/{id}/start", post(start))
        .route("/api/servers/{id}/stop", post(stop))
        .route("/api/servers/{id}/restart", post(restart))
        .route("/api/servers/{id}/command", post(command))
        .route("/api/servers/{id}/logs", get(logs))
        .route("/api/servers/{id}/backups", get(backups).post(backup_now))
        .route("/api/servers/{id}/restore", post(restore))
        .route("/api/servers/{id}/backups/{name}", delete(backup_delete))
        .route("/api/servers/{id}/backups/{name}/download", get(backup_download))
        .route("/api/servers/{id}/settings", get(settings).put(settings_set))
        .route("/api/servers/{id}/settings/raw", put(settings_raw))
        .route("/api/servers/{id}/autostart", put(autostart))
        .route(
            "/api/gamefiles",
            post(gamefiles_upload).layer(DefaultBodyLimit::disable()),
        )
        .route("/api/horizon/build", post(build_horizon))
        .route("/api/password", post(change_password))
        .route("/api/jobs", get(jobs_list))
        .route("/api/jobs/{id}", get(job_one))
        .layer(middleware::from_fn_with_state(app.clone(), guard));
    Router::new()
        .route("/", get(|| async { page(INDEX, "text/html; charset=utf-8") }))
        .route("/app.js", get(|| async { page(APP_JS, "text/javascript; charset=utf-8") }))
        .route("/app.css", get(|| async { page(APP_CSS, "text/css; charset=utf-8") }))
        .route("/icon.svg", get(|| async { page(ICON, "image/svg+xml") }))
        .merge(api)
        .layer(middleware::from_fn(security_headers))
        .with_state(app)
}

fn page(body: &'static str, kind: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, kind),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

async fn security_headers(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'; frame-ancestors 'none'",
        ),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    res
}

/// Alles unter /api/ braucht eine Sitzung; Änderungen zusätzlich den Header `X-Panel: 1` (gegen CSRF).
async fn guard(State(app): St, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_owned();
    let method = req.method().clone();
    if method != Method::GET && method != Method::HEAD {
        let has = req
            .headers()
            .get("x-panel")
            .is_some_and(|v| v.as_bytes() == b"1");
        if !has {
            return err(StatusCode::FORBIDDEN, "X-Panel-Header fehlt");
        }
    }
    if path != "/api/login" {
        let token = auth::token_from_cookies(
            req.headers()
                .get(header::COOKIE)
                .and_then(|v| v.to_str().ok()),
        );
        if !token.is_some_and(|t| app.auth.valid(&t)) {
            return err(StatusCode::UNAUTHORIZED, "Bitte anmelden");
        }
    }
    next.run(req).await
}

#[derive(Deserialize)]
struct LoginBody {
    password: String,
}

async fn login(
    State(app): St,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<LoginBody>,
) -> Response {
    let ip = addr.ip();
    if let Some(secs) = app.auth.locked(ip) {
        return err(
            StatusCode::TOO_MANY_REQUESTS,
            format!("Zu viele Versuche – in {secs} s nochmal."),
        );
    }
    let hash = app.cfg.lock().unwrap().password_hash.clone();
    let good = tokio::task::spawn_blocking(move || auth::verify_password(&hash, &body.password))
        .await
        .unwrap_or(false);
    if !good {
        app.auth.failed(ip);
        tokio::time::sleep(Duration::from_millis(600)).await;
        return err(StatusCode::UNAUTHORIZED, "Falsches Passwort");
    }
    app.auth.succeeded(ip);
    let token = app.auth.new_session();
    (
        [(header::SET_COOKIE, auth::session_cookie(&token))],
        Json(json!({ "ok": true })),
    )
        .into_response()
}

async fn logout(State(app): St, headers: HeaderMap) -> Response {
    if let Some(t) = auth::token_from_cookies(
        headers.get(header::COOKIE).and_then(|v| v.to_str().ok()),
    ) {
        app.auth.end(&t);
    }
    (
        [(header::SET_COOKIE, auth::clear_cookie())],
        Json(json!({ "ok": true })),
    )
        .into_response()
}

#[derive(Deserialize)]
struct PasswordBody {
    old: String,
    new: String,
}

async fn change_password(State(app): St, Json(body): Json<PasswordBody>) -> Response {
    if body.new.chars().count() < 8 {
        return bad("Das neue Passwort braucht mindestens 8 Zeichen.");
    }
    let hash = app.cfg.lock().unwrap().password_hash.clone();
    let old = body.old.clone();
    if !tokio::task::spawn_blocking(move || auth::verify_password(&hash, &old))
        .await
        .unwrap_or(false)
    {
        return err(StatusCode::UNAUTHORIZED, "Altes Passwort stimmt nicht");
    }
    let new_hash = match auth::hash_password(&body.new) {
        Ok(h) => h,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let mut cfg = app.cfg.lock().unwrap();
    cfg.password_hash = new_hash;
    if let Err(e) = cfg.save(&app.data) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    drop(cfg);
    app.auth.end_all();
    ok(json!({ "ok": true, "message": "Passwort geändert – bitte neu anmelden" }))
}

async fn horizon_version(app: &App) -> Option<String> {
    if let Some(v) = app.horizon_version.lock().unwrap().clone() {
        return Some(v);
    }
    let p = &app.mgr.paths.horizon_server;
    if !p.is_file() {
        return None;
    }
    let out = tokio::process::Command::new(p)
        .arg("version")
        .output()
        .await
        .ok()?;
    let v = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_owned();
    if v.is_empty() {
        return None;
    }
    *app.horizon_version.lock().unwrap() = Some(v.clone());
    Some(v)
}

async fn system(State(app): St) -> Response {
    let paths = &app.mgr.paths;
    let gf = paths.game_files.join("horizon-resources.json");
    let game_files = std::fs::metadata(&gf).ok().map(|m| {
        json!({
            "path": paths.game_files,
            "modified": m.modified().ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs()),
        })
    });
    ok(json!({
        "pi": app.sampler.snapshot(&app.data),
        "panel_version": env!("CARGO_PKG_VERSION"),
        "horizon": {
            "server_bin": paths.horizon_server.is_file(),
            "engine_bin": paths.horizon_engine.is_file(),
            "version": horizon_version(&app).await,
            "game_files": game_files,
            "build_script": paths.build_script.is_file(),
            "building": app.jobs.any_running("build"),
        },
        "data_dir": app.data,
    }))
}

async fn list(State(app): St) -> Response {
    let servers: Vec<Value> = app.mgr.all().iter().map(|s| s.info()).collect();
    ok(json!({ "servers": servers }))
}

async fn create(State(app): St, Json(req): Json<CreateRequest>) -> Response {
    match app.mgr.create(req).await {
        Ok(s) => ok(json!({ "ok": true, "server": s.info() })),
        Err(e) => bad(e),
    }
}

macro_rules! server {
    ($app:expr, $id:expr) => {
        match $app.mgr.get(&$id) {
            Some(s) => s,
            None => return err(StatusCode::NOT_FOUND, "Server nicht gefunden"),
        }
    };
}

async fn one(State(app): St, Path(id): Path<String>) -> Response {
    let s = server!(app, id);
    ok(s.info())
}

#[derive(Deserialize)]
struct Confirm {
    confirm: String,
}

async fn remove(State(app): St, Path(id): Path<String>, Json(body): Json<Confirm>) -> Response {
    if body.confirm != id {
        return bad("Zum Löschen den Servernamen genau eintippen.");
    }
    match app.mgr.delete(&id).await {
        Ok(()) => ok(json!({ "ok": true })),
        Err(e) => err(StatusCode::CONFLICT, e),
    }
}

async fn start(State(app): St, Path(id): Path<String>) -> Response {
    let s = server!(app, id);
    result(s.start(&app.mgr.paths).await)
}

async fn stop(State(app): St, Path(id): Path<String>) -> Response {
    let s = server!(app, id);
    result(s.stop().await)
}

async fn restart(State(app): St, Path(id): Path<String>) -> Response {
    let s = server!(app, id);
    let paths = app.mgr.paths.clone();
    // Ein Neustart kann bis zu 200 s Stoppen brauchen: im Hintergrund.
    tokio::spawn(async move {
        if let Err(e) = s.restart(&paths).await {
            s.note(&format!("Neustart fehlgeschlagen: {e}"));
        }
    });
    ok(json!({ "ok": true, "message": "Startet neu" }))
}

#[derive(Deserialize)]
struct CommandBody {
    line: String,
}

async fn command(State(app): St, Path(id): Path<String>, Json(body): Json<CommandBody>) -> Response {
    let s = server!(app, id);
    match s.command(&body.line).await {
        Ok(lines) => ok(json!({ "ok": true, "lines": lines })),
        Err(e) => err(StatusCode::CONFLICT, e),
    }
}

async fn logs(State(app): St, Path(id): Path<String>) -> Response {
    let s = server!(app, id);
    let rx = s.subscribe();
    let backlog = s.backlog();
    let last = backlog.last().map_or(0, |l| l.n);
    let event = |v: Value| Ok::<Event, Infallible>(Event::default().data(v.to_string()));
    let first = tokio_stream::iter(
        backlog
            .into_iter()
            .map(move |l| event(json!(l)))
            .collect::<Vec<_>>(),
    );
    let live = BroadcastStream::new(rx).filter_map(move |r| async move {
        match r {
            Ok(l) if l.n > last => Some(event(json!(l))),
            Ok(_) => None,
            Err(BroadcastStreamRecvError::Lagged(n)) => {
                Some(event(json!({ "n": 0, "text": format!("[Panel] … {n} Zeilen übersprungen") })))
            }
        }
    });
    Sse::new(first.chain(live))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response()
}

async fn backups(State(app): St, Path(id): Path<String>) -> Response {
    let s = server!(app, id);
    ok(json!({ "backups": s.backups() }))
}

#[derive(Deserialize, Default)]
struct BackupBody {
    #[serde(default)]
    name: String,
}

async fn backup_now(State(app): St, Path(id): Path<String>, body: Option<Json<BackupBody>>) -> Response {
    let s = server!(app, id);
    let name = body.map(|b| b.0.name).unwrap_or_default().trim().to_string();
    if name.contains('/') || name.contains("..") || name.contains('\n') {
        return bad("Ungültiger Name");
    }
    let Some(job) = app.jobs.start("backup", format!("Backup von {}", s.id), Some(&s.id)) else {
        return err(StatusCode::CONFLICT, "Für diesen Server läuft schon ein Backup.");
    };
    let program = app.mgr.paths.horizon_server.clone();
    let job_id = job.id;
    tokio::spawn(async move {
        s.note("Backup läuft…");
        let mut args = vec!["backup"];
        if !name.is_empty() {
            args.push(&name);
        }
        let r = horizon::cli(&program, &s.dir, &args, Duration::from_secs(7200)).await;
        s.note(match &r {
            Ok(_) => "Backup fertig",
            Err(_) => "Backup fehlgeschlagen",
        });
        job.finish(r);
    });
    ok(json!({ "ok": true, "job": job_id }))
}

#[derive(Deserialize)]
struct RestoreBody {
    name: String,
}

async fn restore(State(app): St, Path(id): Path<String>, Json(body): Json<RestoreBody>) -> Response {
    let s = server!(app, id);
    if let Err(e) = s.backup_path(&body.name) {
        return bad(e);
    }
    let Some(job) = app.jobs.start("restore", format!("{} wiederherstellen", body.name), Some(&s.id)) else {
        return err(StatusCode::CONFLICT, "Läuft schon.");
    };
    let program = app.mgr.paths.horizon_server.clone();
    let job_id = job.id;
    tokio::spawn(async move {
        s.note(&format!("Stelle {} wieder her…", body.name));
        let r = horizon::cli(
            &program,
            &s.dir,
            &["restore", &body.name, "--yes"],
            Duration::from_secs(7200),
        )
        .await;
        job.finish(r.map(|o| format!("{o}\nDie alte Welt liegt in backups/replaced-…")));
    });
    ok(json!({ "ok": true, "job": job_id }))
}

async fn backup_delete(State(app): St, Path((id, name)): Path<(String, String)>) -> Response {
    let s = server!(app, id);
    match s.backup_path(&name) {
        Ok(p) => match std::fs::remove_file(p) {
            Ok(()) => ok(json!({ "ok": true })),
            Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        },
        Err(e) => bad(e),
    }
}

async fn backup_download(State(app): St, Path((id, name)): Path<(String, String)>) -> Response {
    let s = server!(app, id);
    let path = match s.backup_path(&name) {
        Ok(p) => p,
        Err(e) => return bad(e),
    };
    let file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let len = file.metadata().await.map(|m| m.len()).unwrap_or(0);
    let body = Body::from_stream(tokio_util::io::ReaderStream::new(file));
    (
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (header::CONTENT_LENGTH, len.to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}-{}\"", s.id, name),
            ),
        ],
        body,
    )
        .into_response()
}

async fn settings(State(app): St, Path(id): Path<String>) -> Response {
    let s = server!(app, id);
    match s.settings() {
        Ok(v) => ok(v),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[derive(Deserialize)]
struct SettingsBody {
    changes: Map<String, Value>,
}

async fn settings_set(State(app): St, Path(id): Path<String>, Json(body): Json<SettingsBody>) -> Response {
    let s = server!(app, id);
    match app.mgr.set_settings(&s, &body.changes).await {
        Ok(out) => ok(json!({ "ok": true, "message": out })),
        Err(e) => bad(e),
    }
}

#[derive(Deserialize)]
struct RawBody {
    text: String,
}

async fn settings_raw(State(app): St, Path(id): Path<String>, Json(body): Json<RawBody>) -> Response {
    let s = server!(app, id);
    match app.mgr.set_raw(&s, &body.text) {
        Ok(()) => ok(json!({ "ok": true, "message": "Gespeichert – gilt ab dem nächsten Start." })),
        Err(e) => bad(e),
    }
}

#[derive(Deserialize)]
struct AutostartBody {
    autostart: bool,
}

async fn autostart(State(app): St, Path(id): Path<String>, Json(body): Json<AutostartBody>) -> Response {
    let s = server!(app, id);
    match s.set_autostart(body.autostart) {
        Ok(()) => ok(json!({ "ok": true })),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// Spieldateien als ZIP hochladen (der Ordner 26.3 von horizon-prepare).
async fn gamefiles_upload(State(app): St, body: Body) -> Response {
    if app.mgr.all().iter().any(|s| s.alive()) {
        return err(StatusCode::CONFLICT, "Erst alle Server stoppen.");
    }
    if app.jobs.any_running("gamefiles") {
        return err(StatusCode::CONFLICT, "Ein Upload läuft schon.");
    }
    let uploads = app.data.join("uploads");
    if let Err(e) = tokio::fs::create_dir_all(&uploads).await {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    let zip = uploads.join("gamefiles.zip");
    let mut file = match tokio::fs::File::create(&zip).await {
        Ok(f) => f,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let mut stream = body.into_data_stream();
    let mut total: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                let _ = tokio::fs::remove_file(&zip).await;
                return bad(format!("Upload abgebrochen: {e}"));
            }
        };
        total += chunk.len() as u64;
        if total > 8 * 1024 * 1024 * 1024 {
            let _ = tokio::fs::remove_file(&zip).await;
            return bad("Datei zu groß (über 8 GB)");
        }
        if let Err(e) = file.write_all(&chunk).await {
            return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
        }
    }
    let _ = file.flush().await;
    drop(file);
    let Some(job) = app.jobs.start("gamefiles", "Spieldateien einspielen", None) else {
        return err(StatusCode::CONFLICT, "Läuft schon.");
    };
    let job_id = job.id;
    let target = app.mgr.paths.game_files.clone();
    let data = app.data.clone();
    tokio::spawn(async move {
        job.line(format!("{} MB empfangen, entpacke…", total / 1_000_000));
        let r = import_game_files(&job, &zip, &data, &target).await;
        let _ = tokio::fs::remove_file(&zip).await;
        job.finish(r);
    });
    ok(json!({ "ok": true, "job": job_id }))
}

async fn import_game_files(
    job: &jobs::Handle,
    zip: &std::path::Path,
    data: &std::path::Path,
    target: &std::path::Path,
) -> Result<String, String> {
    let work = data.join("uploads/gamefiles-unpack");
    let _ = tokio::fs::remove_dir_all(&work).await;
    tokio::fs::create_dir_all(&work)
        .await
        .map_err(|e| e.to_string())?;
    let mut cmd = tokio::process::Command::new("unzip");
    cmd.arg("-q").arg("-o").arg(zip).arg("-d").arg(&work);
    jobs::run_logged(job, cmd)
        .await
        .map_err(|e| format!("Entpacken fehlgeschlagen ({e}). Ist es eine ZIP-Datei? Ist unzip installiert?"))?;
    let found = tokio::task::spawn_blocking({
        let work = work.clone();
        move || find_manifest(&work, 4)
    })
    .await
    .map_err(|e| e.to_string())?
    .ok_or("Im ZIP ist keine horizon-resources.json – bitte den Ordner 26.3 zippen, den horizon-prepare erstellt.")?;
    job.line(format!("Gefunden: {}", found.display()));
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
    }
    let old = target.with_extension("old");
    let _ = tokio::fs::remove_dir_all(&old).await;
    if target.exists() {
        tokio::fs::rename(target, &old)
            .await
            .map_err(|e| e.to_string())?;
    }
    tokio::fs::rename(&found, target)
        .await
        .map_err(|e| e.to_string())?;
    let _ = tokio::fs::remove_dir_all(&old).await;
    let _ = tokio::fs::remove_dir_all(&work).await;
    Ok(format!("Spieldateien liegen jetzt in {}", target.display()))
}

fn find_manifest(dir: &std::path::Path, depth: u32) -> Option<PathBuf> {
    if dir.join("horizon-resources.json").is_file() {
        return Some(dir.to_path_buf());
    }
    if depth == 0 {
        return None;
    }
    let mut entries: Vec<_> = std::fs::read_dir(dir).ok()?.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    entries
        .iter()
        .filter(|e| e.path().is_dir())
        .find_map(|e| find_manifest(&e.path(), depth - 1))
}

async fn build_horizon(State(app): St) -> Response {
    let script = app.mgr.paths.build_script.clone();
    if !script.is_file() {
        return err(
            StatusCode::CONFLICT,
            format!("Build-Skript fehlt: {}", script.display()),
        );
    }
    let Some(job) = app.jobs.start("build", "Horizon bauen (dauert auf dem Pi lange)", None) else {
        return err(StatusCode::CONFLICT, "Der Build läuft schon.");
    };
    let job_id = job.id;
    let data = app.data.clone();
    let app2 = app.clone();
    tokio::spawn(async move {
        let mut cmd = tokio::process::Command::new("bash");
        cmd.arg(&script).env("HORIZON_PANEL_DIR", &data);
        let r = jobs::run_logged(&job, cmd).await;
        *app2.horizon_version.lock().unwrap() = None;
        let running = app2.mgr.all().iter().any(|s| s.alive());
        job.finish(r.map(|_| {
            if running {
                "Fertig. Laufende Server benutzen die neue Version nach einem Neustart.".into()
            } else {
                "Fertig.".into()
            }
        }));
    });
    ok(json!({ "ok": true, "job": job_id }))
}

async fn jobs_list(State(app): St) -> Response {
    ok(json!({ "jobs": app.jobs.summary() }))
}

async fn job_one(State(app): St, Path(id): Path<u64>) -> Response {
    match app.jobs.get(id) {
        Some(j) => ok(json!(j)),
        None => err(StatusCode::NOT_FOUND, "Aufgabe nicht gefunden"),
    }
}
