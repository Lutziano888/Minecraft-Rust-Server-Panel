//! Längere Aufgaben im Hintergrund (Backups, Horizon bauen, Spieldateien entpacken),
//! deren Fortschritt die Oberfläche abfragt.

use serde::Serialize;
use std::{
    collections::VecDeque,
    process::Stdio,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};

const KEEP: usize = 30;
const MAX_LINES: usize = 3000;

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Running,
    Ok,
    Failed,
}

#[derive(Serialize, Clone)]
pub struct Job {
    pub id: u64,
    pub kind: String,
    pub title: String,
    pub server: Option<String>,
    pub state: State,
    pub started: u64,
    pub finished: Option<u64>,
    pub lines: Vec<String>,
}

#[derive(Default)]
pub struct Jobs {
    next: AtomicU64,
    list: Mutex<VecDeque<Job>>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[derive(Clone)]
pub struct Handle {
    jobs: Arc<Jobs>,
    pub id: u64,
}

impl Handle {
    pub fn line(&self, text: impl Into<String>) {
        let text = text.into();
        self.jobs.edit(self.id, |j| {
            j.lines.push(text);
            if j.lines.len() > MAX_LINES {
                let cut = j.lines.len() - MAX_LINES;
                j.lines.drain(..cut);
            }
        });
    }

    pub fn finish(&self, result: Result<String, String>) {
        let (state, msg) = match result {
            Ok(m) => (State::Ok, m),
            Err(m) => (State::Failed, m),
        };
        self.jobs.edit(self.id, |j| {
            if !msg.is_empty() {
                j.lines.extend(msg.lines().map(str::to_owned));
            }
            j.state = state;
            j.finished = Some(now());
        });
    }
}

impl Jobs {
    /// Eine neue Aufgabe. `None`, wenn schon eine gleiche (`kind` + Server) läuft.
    pub fn start(
        self: &Arc<Self>,
        kind: &str,
        title: impl Into<String>,
        server: Option<&str>,
    ) -> Option<Handle> {
        let mut list = self.list.lock().unwrap();
        if list.iter().any(|j| {
            j.state == State::Running && j.kind == kind && j.server.as_deref() == server
        }) {
            return None;
        }
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        list.push_back(Job {
            id,
            kind: kind.into(),
            title: title.into(),
            server: server.map(str::to_owned),
            state: State::Running,
            started: now(),
            finished: None,
            lines: Vec::new(),
        });
        while list.len() > KEEP {
            if let Some(pos) = list.iter().position(|j| j.state != State::Running) {
                list.remove(pos);
            } else {
                break;
            }
        }
        Some(Handle {
            jobs: self.clone(),
            id,
        })
    }

    fn edit(&self, id: u64, f: impl FnOnce(&mut Job)) {
        if let Some(j) = self.list.lock().unwrap().iter_mut().find(|j| j.id == id) {
            f(j);
        }
    }

    pub fn get(&self, id: u64) -> Option<Job> {
        self.list.lock().unwrap().iter().find(|j| j.id == id).cloned()
    }

    /// Alle Aufgaben, neueste zuerst, ohne ihre Ausgabe.
    pub fn summary(&self) -> Vec<Job> {
        self.list
            .lock()
            .unwrap()
            .iter()
            .rev()
            .map(|j| Job {
                lines: j.lines.last().cloned().into_iter().collect(),
                ..j.clone()
            })
            .collect()
    }

    pub fn any_running(&self, kind: &str) -> bool {
        self.list
            .lock()
            .unwrap()
            .iter()
            .any(|j| j.kind == kind && j.state == State::Running)
    }
}

/// Ein Programm ausführen und jede Ausgabezeile in die Aufgabe schreiben.
pub async fn run_logged(job: &Handle, mut cmd: Command) -> Result<(), String> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let out = child.stdout.take().unwrap();
    let err = child.stderr.take().unwrap();
    let j1 = job.clone();
    let t1 = tokio::spawn(async move {
        let mut lines = BufReader::new(out).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            j1.line(l);
        }
    });
    let j2 = job.clone();
    let t2 = tokio::spawn(async move {
        let mut lines = BufReader::new(err).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            j2.line(l);
        }
    });
    let status = child.wait().await.map_err(|e| e.to_string())?;
    let _ = t1.await;
    let _ = t2.await;
    if status.success() {
        Ok(())
    } else {
        Err(format!("beendet mit {status}"))
    }
}
