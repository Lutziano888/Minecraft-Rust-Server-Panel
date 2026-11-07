//! panel.toml im Datenordner und die Pfade, die das Panel benutzt.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const FILE: &str = "panel.toml";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PanelConfig {
    /// Wo das Panel lauscht. 0.0.0.0:5341 = im ganzen Heimnetz erreichbar (Passwort nötig).
    #[serde(default = "default_bind")]
    pub bind: String,
    /// Argon2-Hash des Passworts (`horizon-pi-panel set-password`).
    #[serde(default)]
    pub password_hash: String,
    /// Leer = Standardpfad im Datenordner.
    #[serde(default)]
    pub horizon_server: String,
    #[serde(default)]
    pub horizon_engine: String,
    #[serde(default)]
    pub game_files: String,
    #[serde(default)]
    pub build_script: String,
    /// Erster Port für neue Server.
    #[serde(default = "default_first_port")]
    pub first_port: u16,
}

fn default_bind() -> String {
    "0.0.0.0:5341".into()
}
fn default_first_port() -> u16 {
    25565
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            password_hash: String::new(),
            horizon_server: String::new(),
            horizon_engine: String::new(),
            game_files: String::new(),
            build_script: String::new(),
            first_port: default_first_port(),
        }
    }
}

impl PanelConfig {
    pub fn load(data: &Path) -> Result<Self, String> {
        let path = data.join(FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub fn save(&self, data: &Path) -> Result<(), String> {
        std::fs::create_dir_all(data).map_err(|e| e.to_string())?;
        let path = data.join(FILE);
        let text = format!(
            "# Einstellungen von horizon-pi-panel. Nach Änderungen: sudo systemctl restart horizon-pi-panel\n{}",
            toml::to_string_pretty(self).map_err(|e| e.to_string())?
        );
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }
}

/// Alle Pfade, fertig aufgelöst.
#[derive(Clone, Debug)]
pub struct Paths {
    pub data: PathBuf,
    pub servers: PathBuf,
    pub horizon_server: PathBuf,
    pub horizon_engine: PathBuf,
    pub game_files: PathBuf,
    pub build_script: PathBuf,
}

impl Paths {
    pub fn new(data: &Path, cfg: &PanelConfig) -> Self {
        let pick = |value: &str, default: PathBuf| {
            if value.trim().is_empty() {
                default
            } else {
                PathBuf::from(value)
            }
        };
        Self {
            data: data.to_path_buf(),
            servers: data.join("servers"),
            horizon_server: pick(&cfg.horizon_server, data.join("bin/horizon-server")),
            horizon_engine: pick(&cfg.horizon_engine, data.join("bin/horizon-engine")),
            game_files: pick(&cfg.game_files, data.join("game-files/26.3")),
            build_script: pick(&cfg.build_script, data.join("scripts/build-horizon.sh")),
        }
    }
}

/// Der Datenordner: --data, sonst $HORIZON_PANEL_DIR, sonst ~/horizon-panel.
pub fn data_dir(flag: Option<String>) -> PathBuf {
    if let Some(d) = flag {
        return PathBuf::from(d);
    }
    if let Some(d) = std::env::var_os("HORIZON_PANEL_DIR") {
        return PathBuf::from(d);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join("horizon-panel")
}
