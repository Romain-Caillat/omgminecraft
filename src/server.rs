//! Le processus Java du serveur Minecraft : un seul à la fois, puisqu'ils
//! partagent tous le port ouvert par NetBird.

use std::{
    collections::VecDeque,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{bail, Result};
use serde::Serialize;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader},
    process::{ChildStdin, Command},
    sync::{broadcast, oneshot, watch},
};

use crate::instances::now;

const HISTORY: usize = 2000;

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum RunState {
    #[default]
    Stopped,
    Starting,
    Running,
    Stopping,
}

#[derive(Serialize, Clone, Default, Debug)]
pub struct Status {
    pub state: RunState,
    /// Instance en cours, ou la dernière lancée une fois arrêtée.
    pub instance: Option<String>,
    pub started_at: Option<u64>,
    /// Monde chargé par le processus en cours (le `level-name` au démarrage).
    pub world: Option<String>,
    pub players: Vec<String>,
    pub last_exit: Option<String>,
}

pub struct Server {
    status: watch::Sender<Status>,
    logs: broadcast::Sender<String>,
    history: Mutex<VecDeque<String>>,
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    kill: Mutex<Option<oneshot::Sender<()>>>,
}

impl Server {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            status: watch::Sender::new(Status::default()),
            logs: broadcast::channel(1024).0,
            history: Mutex::new(VecDeque::with_capacity(HISTORY)),
            stdin: tokio::sync::Mutex::new(None),
            kill: Mutex::new(None),
        })
    }

    pub fn status(&self) -> Status {
        self.status.borrow().clone()
    }

    pub fn watch_status(&self) -> watch::Receiver<Status> {
        self.status.subscribe()
    }

    /// Instance dont le processus tourne (quel que soit son état de démarrage).
    pub fn running_instance(&self) -> Option<String> {
        let s = self.status.borrow();
        (s.state != RunState::Stopped).then(|| s.instance.clone()).flatten()
    }

    /// Monde en cours d'utilisation si `instance` tourne.
    pub fn live_world(&self, instance: &str) -> Option<String> {
        let s = self.status.borrow();
        (s.state != RunState::Stopped && s.instance.as_deref() == Some(instance))
            .then(|| s.world.clone())
            .flatten()
    }

    pub fn log(&self, line: impl Into<String>) {
        let line = line.into();
        {
            let mut h = self.history.lock().unwrap();
            if h.len() >= HISTORY {
                h.pop_front();
            }
            h.push_back(line.clone());
        }
        let _ = self.logs.send(line);
    }

    /// L'abonnement est pris avant la copie de l'historique : au pire une ligne
    /// en double, jamais un trou.
    pub fn subscribe_logs(&self) -> (Vec<String>, broadcast::Receiver<String>) {
        let rx = self.logs.subscribe();
        let h = self.history.lock().unwrap().iter().cloned().collect();
        (h, rx)
    }

    pub async fn send_command(&self, cmd: &str) -> Result<()> {
        let mut guard = self.stdin.lock().await;
        let Some(stdin) = guard.as_mut() else { bail!("aucun serveur ne tourne") };
        stdin.write_all(format!("{cmd}\n").as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }

    pub async fn start(self: &Arc<Self>, instance: String, world: String, mut cmd: Command) -> Result<()> {
        if self.status.borrow().state != RunState::Stopped {
            bail!("un serveur tourne déjà");
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn()?;
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");
        *self.stdin.lock().await = child.stdin.take();
        let (kill_tx, kill_rx) = oneshot::channel();
        *self.kill.lock().unwrap() = Some(kill_tx);

        self.log(format!("[omg] démarrage de « {instance} » (pid {})", child.id().unwrap_or(0)));
        self.status.send_replace(Status {
            state: RunState::Starting,
            instance: Some(instance),
            started_at: Some(now()),
            world: Some(world),
            players: vec![],
            last_exit: None,
        });

        tokio::spawn(self.clone().read_output(stdout));
        tokio::spawn(self.clone().read_output(stderr));

        let me = self.clone();
        tokio::spawn(async move {
            let waited = tokio::select! {
                r = child.wait() => Some(r),
                Ok(()) = kill_rx => None,
            };
            let res = match waited {
                Some(r) => r,
                None => {
                    me.log("[omg] le serveur ne répond pas, arrêt forcé");
                    let _ = child.start_kill();
                    child.wait().await
                }
            };
            *me.stdin.lock().await = None;
            me.kill.lock().unwrap().take();
            let expected = me.status.borrow().state == RunState::Stopping;
            let desc = match res {
                Ok(s) if s.success() && expected => "arrêt normal".to_string(),
                Ok(s) if s.success() => "arrêté depuis la console".to_string(),
                Ok(s) => format!("planté ({s})"),
                Err(e) => format!("erreur d'attente : {e}"),
            };
            me.log(format!("[omg] serveur arrêté : {desc}"));
            me.status.send_modify(|s| {
                s.state = RunState::Stopped;
                s.started_at = None;
                s.world = None;
                s.players.clear();
                s.last_exit = Some(desc);
            });
        });
        Ok(())
    }

    async fn read_output(self: Arc<Self>, out: impl AsyncRead + Unpin) {
        let mut reader = BufReader::new(out);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let line = String::from_utf8_lossy(&buf).trim_end().to_string();
            self.observe(&line);
            self.log(line);
        }
    }

    /// Lit l'état du serveur dans ses logs : fin du chargement, connexions.
    fn observe(&self, line: &str) {
        let msg = line.rsplit_once("]: ").map(|(_, m)| m).unwrap_or(line).trim();
        if msg.starts_with("Done (") {
            self.status.send_if_modified(|s| {
                let changed = s.state == RunState::Starting;
                if changed {
                    s.state = RunState::Running;
                }
                changed
            });
        } else if let Some(name) = msg.strip_suffix(" joined the game").filter(|n| is_player(n)) {
            let name = name.to_string();
            self.status.send_modify(|s| {
                if !s.players.contains(&name) {
                    s.players.push(name);
                }
            });
        } else if let Some(name) = msg.strip_suffix(" left the game").filter(|n| is_player(n)) {
            self.status.send_modify(|s| s.players.retain(|p| p != name));
        }
    }

    /// `stop` dans la console, puis arrêt forcé passé le délai.
    pub async fn stop(&self, timeout: Duration) {
        let running = self.status.send_if_modified(|s| {
            let r = s.state != RunState::Stopped;
            if r {
                s.state = RunState::Stopping;
            }
            r
        });
        if !running {
            return;
        }
        let _ = self.send_command("stop").await;
        let mut rx = self.status.subscribe();
        let graceful = tokio::time::timeout(timeout, async {
            rx.wait_for(|s| s.state == RunState::Stopped).await.is_ok()
        })
        .await
        .is_ok();
        if !graceful {
            if let Some(k) = self.kill.lock().unwrap().take() {
                let _ = k.send(());
            }
            let _ = rx.wait_for(|s| s.state == RunState::Stopped).await.is_ok();
        }
    }
}

/// Un pseudo Minecraft : une ligne de chat (`<pseudo> ...`) ne peut pas en être un.
fn is_player(n: &str) -> bool {
    (1..=16).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_players_and_ready() {
        let s = Server::new();
        s.status.send_modify(|st| st.state = RunState::Starting);
        s.observe("[12:00:00] [Server thread/INFO]: Done (3.2s)! For help, type \"help\"");
        assert_eq!(s.status().state, RunState::Running);
        s.observe("[12:00:01] [Server thread/INFO]: Steve joined the game");
        s.observe("[12:00:02] [Server thread/INFO]: <Alex> Bob joined the game");
        assert_eq!(s.status().players, vec!["Steve"]);
        s.observe("[12:00:03] [Server thread/INFO]: Steve left the game");
        assert!(s.status().players.is_empty());
    }
}
