//! horizon-pi-panel: ein Web-Panel für Horizon-Server (Minecraft Java 26.3 in Rust) auf dem Raspberry Pi 5.

mod auth;
mod config;
mod horizon;
mod jobs;
mod routes;
mod servers;
mod system;

use std::{net::SocketAddr, sync::Arc};

const HELP: &str = "horizon-pi-panel – Web-Panel für Horizon-Server

Benutzung:
  horizon-pi-panel [--data ORDNER] serve          Panel starten (Standard)
  horizon-pi-panel [--data ORDNER] set-password   Passwort setzen (liest von der Tastatur oder stdin)
  horizon-pi-panel [--data ORDNER] paths          Pfade anzeigen
  horizon-pi-panel --version

Datenordner: --data, sonst $HORIZON_PANEL_DIR, sonst ~/horizon-panel";

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut data = None;
    if let Some(i) = args.iter().position(|a| a == "--data") {
        if i + 1 >= args.len() {
            fail("--data braucht einen Ordner");
        }
        data = Some(args.remove(i + 1));
        args.remove(i);
    }
    let data = config::data_dir(data);
    match args.first().map(String::as_str) {
        None | Some("serve") => serve(data),
        Some("set-password") => set_password(&data),
        Some("paths") => {
            let cfg = config::PanelConfig::load(&data).unwrap_or_else(|e| fail(&e));
            let p = config::Paths::new(&data, &cfg);
            println!("data           {}", p.data.display());
            println!("servers        {}", p.servers.display());
            println!("horizon-server {}", p.horizon_server.display());
            println!("horizon-engine {}", p.horizon_engine.display());
            println!("game-files     {}", p.game_files.display());
            println!("build-script   {}", p.build_script.display());
            println!("bind           {}", cfg.bind);
        }
        Some("-V" | "--version" | "version") => println!("horizon-pi-panel {}", env!("CARGO_PKG_VERSION")),
        Some("-h" | "--help" | "help") => println!("{HELP}"),
        Some(other) => fail(&format!("Unbekannter Befehl {other}\n\n{HELP}")),
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("horizon-pi-panel: {msg}");
    std::process::exit(1)
}

fn set_password(data: &std::path::Path) {
    use std::io::IsTerminal;
    let password = if std::io::stdin().is_terminal() {
        let a = rpassword::prompt_password("Neues Panel-Passwort: ").unwrap_or_else(|e| fail(&e.to_string()));
        let b = rpassword::prompt_password("Nochmal: ").unwrap_or_else(|e| fail(&e.to_string()));
        if a != b {
            fail("Die Passwörter sind verschieden.");
        }
        a
    } else {
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .unwrap_or_else(|e| fail(&e.to_string()));
        line.trim_end_matches(['\r', '\n']).to_string()
    };
    if password.chars().count() < 8 {
        fail("Das Passwort braucht mindestens 8 Zeichen.");
    }
    let mut cfg = config::PanelConfig::load(data).unwrap_or_else(|e| fail(&e));
    cfg.password_hash = auth::hash_password(&password).unwrap_or_else(|e| fail(&e));
    cfg.save(data).unwrap_or_else(|e| fail(&e));
    println!("Passwort gespeichert in {}", data.join(config::FILE).display());
}

#[tokio::main]
async fn serve(data: std::path::PathBuf) {
    let cfg = config::PanelConfig::load(&data).unwrap_or_else(|e| fail(&e));
    if cfg.password_hash.is_empty() {
        fail("Noch kein Passwort gesetzt: horizon-pi-panel set-password");
    }
    if !data.join(config::FILE).exists() {
        let _ = cfg.save(&data);
    }
    let paths = config::Paths::new(&data, &cfg);
    let bind: SocketAddr = cfg
        .bind
        .parse()
        .unwrap_or_else(|_| fail(&format!("bind = \"{}\" ist keine Adresse wie 0.0.0.0:8080", cfg.bind)));
    let mgr = servers::Manager::load(paths, cfg.first_port);
    let sampler = Arc::new(system::Sampler::default());
    sampler.clone().spawn();
    let app = Arc::new(routes::App {
        mgr: mgr.clone(),
        auth: auth::Auth::default(),
        jobs: Arc::new(jobs::Jobs::default()),
        sampler,
        cfg: std::sync::Mutex::new(cfg),
        data: data.clone(),
        horizon_version: std::sync::Mutex::new(None),
    });
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .unwrap_or_else(|e| fail(&format!("{bind}: {e}")));
    eprintln!(
        "horizon-pi-panel {} auf http://{bind} (Daten: {})",
        env!("CARGO_PKG_VERSION"),
        data.display()
    );
    mgr.spawn_poller();
    {
        let mgr = mgr.clone();
        tokio::spawn(async move { mgr.autostart().await });
    }
    let router = routes::router(app);
    tokio::spawn(async move {
        if let Err(e) = axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        {
            fail(&e.to_string());
        }
    });
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("SIGTERM");
    tokio::select! {
        _ = term.recv() => {},
        _ = tokio::signal::ctrl_c() => {},
    }
    // Jeden Horizon-Server speichern und stoppen, dann beenden (offene Konsolen-Streams egal).
    eprintln!("Panel wird beendet – stoppe alle Server sauber…");
    mgr.stop_all().await;
    eprintln!("Alle Server gestoppt.");
    std::process::exit(0);
}
