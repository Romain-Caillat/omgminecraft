use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::{
    config::Config,
    error::{ApiResult, AppError},
    install,
    instances::{now, write_atomic, InstallStatus},
    properties::Properties,
    server::Server,
};

pub const STOP_TIMEOUT: Duration = Duration::from_secs(60);

/// Ce qui doit survivre à un redémarrage du panel : quelle instance tourne.
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Persisted {
    pub active: Option<String>,
    /// Vrai tant que l'utilisateur n'a pas arrêté le serveur : au démarrage du
    /// panel (reboot du conteneur), l'instance active est relancée.
    pub running: bool,
}

pub struct App {
    pub cfg: Config,
    pub http: reqwest::Client,
    pub server: Arc<Server>,
    pub shutdown: CancellationToken,
    pub java_lock: tokio::sync::Mutex<()>,
    /// Sérialise démarrages, arrêts et restaurations.
    pub(crate) op: tokio::sync::Mutex<()>,
    /// Une sauvegarde à la fois.
    pub(crate) backup_lock: tokio::sync::Mutex<()>,
    pub(crate) backup_state: Mutex<crate::backups::BackupState>,
    /// Une construction de zip de pack client à la fois.
    pub(crate) share_lock: tokio::sync::Mutex<()>,
    persisted: Mutex<Persisted>,
    tmp_counter: AtomicU64,
}

impl App {
    pub async fn init(cfg: Config) -> Result<Arc<Self>> {
        for d in ["instances", "java", "tmp"] {
            std::fs::create_dir_all(cfg.data_dir.join(d))
                .with_context(|| format!("création de {}", cfg.data_dir.join(d).display()))?;
        }
        // Restes d'un arrêt brutal : téléchargements et archives à moitié écrits.
        for e in std::fs::read_dir(cfg.data_dir.join("tmp"))?.flatten() {
            let _ = std::fs::remove_file(e.path()).or_else(|_| std::fs::remove_dir_all(e.path()));
        }
        std::fs::create_dir_all(&cfg.backup_dir)
            .with_context(|| format!("création de {}", cfg.backup_dir.display()))?;
        let backup_state = App::load_backup_state(&cfg.backup_dir);
        let persisted = std::fs::read(cfg.data_dir.join("omg.json"))
            .ok()
            .and_then(|r| serde_json::from_slice(&r).ok())
            .unwrap_or_default();
        let http = reqwest::Client::builder()
            .user_agent(concat!("omgminecraft/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .build()?;
        Ok(Arc::new(Self {
            cfg,
            http,
            server: Server::new(),
            shutdown: CancellationToken::new(),
            java_lock: Default::default(),
            op: Default::default(),
            backup_lock: Default::default(),
            backup_state: Mutex::new(backup_state),
            share_lock: Default::default(),
            persisted: Mutex::new(persisted),
            tmp_counter: AtomicU64::new(0),
        }))
    }

    pub fn instances_dir(&self) -> PathBuf {
        self.cfg.data_dir.join("instances")
    }
    pub fn instance_dir(&self, id: &str) -> PathBuf {
        self.instances_dir().join(id)
    }
    pub fn java_dir(&self) -> PathBuf {
        self.cfg.data_dir.join("java")
    }
    /// Chemin unique dans `tmp/`, sur le même système de fichiers que les
    /// instances pour que les `rename` finaux soient atomiques.
    pub fn tmp_path(&self, ext: &str) -> PathBuf {
        let n = self.tmp_counter.fetch_add(1, Ordering::Relaxed);
        self.cfg.data_dir.join("tmp").join(format!("{}-{n}.{ext}", now()))
    }

    pub fn persisted(&self) -> Persisted {
        self.persisted.lock().unwrap().clone()
    }

    fn persist(&self, f: impl FnOnce(&mut Persisted)) {
        let mut p = self.persisted.lock().unwrap();
        f(&mut p);
        if let Ok(data) = serde_json::to_vec_pretty(&*p) {
            if let Err(e) = write_atomic(&self.cfg.data_dir.join("omg.json"), &data) {
                tracing::error!("omg.json : {e}");
            }
        }
    }

    pub fn forget_instance(&self, id: &str) {
        self.persist(|p| {
            if p.active.as_deref() == Some(id) {
                *p = Persisted::default();
            }
        });
    }

    /// Au démarrage du panel : solde les installations interrompues et relance
    /// l'instance qui tournait.
    pub fn boot(self: &Arc<Self>) {
        if let Ok(list) = self.list_instances() {
            for i in list.into_iter().filter(|i| i.status == InstallStatus::Installing) {
                let _ = self.update_instance(&i.id, |i| {
                    i.status = InstallStatus::Error;
                    i.error = Some("installation interrompue par un redémarrage du panel".into());
                });
            }
        }
        self.enforce_all();
        self.backfill_properties();
        self.spawn_backups();
        self.spawn_crash_hints();
        let p = self.persisted();
        if let (true, Some(id)) = (p.running, p.active) {
            let app = self.clone();
            tokio::spawn(async move {
                if let Err(e) = app.start_instance(&id).await {
                    app.server.log(format!("[omg] relance de « {id} » impossible : {}", e.1));
                }
            });
        }
    }

    /// Réglages qui ne dépendent pas de l'instance :
    /// - une seule adresse pour toutes : le port est celui qu'ouvre la policy
    ///   NetBird, et le serveur écoute sur toutes les interfaces ;
    /// - pas de whitelist : qui peut jouer, c'est NetBird qui le décide.
    pub fn enforce_props(&self, props: &mut Properties) {
        props.set("server-port", &self.cfg.game_port.to_string());
        props.set("server-ip", "");
        props.set("white-list", "false");
        props.set("enforce-whitelist", "false");
    }

    /// Applique `enforce_props` aux fichiers déjà présents, pour que l'éditeur
    /// montre les vraies valeurs sans attendre un démarrage.
    fn enforce_all(&self) {
        let Ok(list) = self.list_instances() else { return };
        for inst in list {
            let path = self.instance_dir(&inst.id).join("server.properties");
            let Ok(mut props) = Properties::load(&path) else { continue };
            if props.len() == 0 {
                continue;
            }
            self.enforce_props(&mut props);
            if let Err(e) = props.save(&path) {
                tracing::warn!("{}: {e}", path.display());
            }
        }
    }

    /// Instances créées avant que l'installation ne génère `server.properties` :
    /// on le complète une fois, en tâche de fond.
    fn backfill_properties(self: &Arc<Self>) {
        let Ok(list) = self.list_instances() else { return };
        let p = self.persisted();
        for inst in list {
            // Celle qui va être relancée génère son fichier en démarrant.
            if p.running && p.active.as_deref() == Some(inst.id.as_str()) {
                continue;
            }
            if inst.status != InstallStatus::Ready || inst.kind == crate::instances::Kind::Custom {
                continue;
            }
            // `logs/` existe dès que Java a tourné une fois dans l'instance :
            // une version trop ancienne pour `--initSettings` n'est tentée qu'une fois.
            if self.instance_dir(&inst.id).join("logs").exists() {
                continue;
            }
            let props = Properties::load(&self.instance_dir(&inst.id).join("server.properties"));
            if props.map(|p| p.len() >= 10).unwrap_or(true) {
                continue;
            }
            let app = self.clone();
            tokio::spawn(async move {
                let res = async {
                    let java = install::ensure_java(&app, inst.java_major).await?;
                    install::init_settings(&app, &inst, &java).await
                }
                .await;
                if let Err(e) = res {
                    app.server.log(format!("[omg] [{}] server.properties : {e:#}", inst.id));
                }
            });
        }
    }

    /// Démarre `id`, en arrêtant d'abord l'instance en cours s'il y en a une.
    pub async fn start_instance(&self, id: &str) -> ApiResult<()> {
        let _op = self.op.lock().await;
        let inst = self.load_instance(id)?;
        match inst.status {
            InstallStatus::Ready => {}
            InstallStatus::Installing => return Err(AppError::conflict("installation en cours")),
            InstallStatus::Error => {
                return Err(AppError::conflict(format!(
                    "installation en échec : {}",
                    inst.error.unwrap_or_default()
                )))
            }
        }
        let dir = self.instance_dir(id);
        if !dir.join(&inst.jar).exists() {
            return Err(AppError::conflict(format!("{} introuvable dans l'instance", inst.jar)));
        }
        if self.server.running_instance().is_some() {
            self.server.stop(STOP_TIMEOUT).await;
        }
        let java = install::ensure_java(self, inst.java_major).await?;

        let props_path = dir.join("server.properties");
        let mut props = Properties::load(&props_path)?;
        self.enforce_props(&mut props);
        props.save(&props_path)?;
        let world = props.get("level-name").filter(|w| !w.is_empty()).unwrap_or("world").to_string();

        let mut cmd = Command::new(java);
        cmd.current_dir(&dir)
            .arg(format!("-Xms{}M", inst.memory_min_mb))
            .arg(format!("-Xmx{}M", inst.memory_max_mb))
            // Log4Shell : inoffensif sur les versions récentes, utile sur 1.17–1.18.
            .arg("-Dlog4j2.formatMsgNoLookups=true")
            .args(&inst.jvm_args)
            .args(install::launch_target(&inst.jar))
            .arg("nogui");
        self.server.start(id.to_string(), world, cmd).await?;
        self.persist(|p| {
            p.active = Some(id.to_string());
            p.running = true;
        });
        Ok(())
    }

    /// `remember = false` au moment d'éteindre le panel : l'instance repartira
    /// au prochain démarrage.
    pub async fn stop_server(&self, remember: bool) {
        let _op = self.op.lock().await;
        self.server.stop(STOP_TIMEOUT).await;
        if remember {
            self.persist(|p| p.running = false);
        }
    }
}
