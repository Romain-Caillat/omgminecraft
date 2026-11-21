//! Sauvegardes des instances : `<backup_dir>/<instance>/<horodatage>-<type>.tar.gz`.
//!
//! - **auto** : chaque nuit (à `backup_hour`, ou au premier passage après si le
//!   panel était éteint), seulement pour les instances où un joueur s'est
//!   connecté depuis la dernière sauvegarde. Gardées `retention_days` jours,
//!   la plus récente de chaque instance étant toujours conservée : une instance
//!   délaissée ne perd jamais sa dernière copie.
//! - **manuel** : à la demande, gardées jusqu'à suppression.
//! - **avant-restauration** : prise automatiquement avant chaque restauration,
//!   même durée de vie que les auto.
//!
//! Le dossier de sauvegarde est monté à part (`mp1` sur PCT 107) : effacer les
//! données ne l'emporte pas avec elles.

use std::{
    collections::BTreeMap,
    io,
    path::{Path as FsPath, PathBuf},
    sync::Arc,
    time::{Duration, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, StatusCode},
    response::Response,
    Json,
};
use chrono::{Local, Timelike};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_util::io::ReaderStream;

use crate::{
    app::App,
    error::{ApiResult, AppError},
    instances::{valid_id, write_atomic, InstallStatus},
    server::RunState,
};

type AppState = State<Arc<App>>;

/// Sans valeur ou régénérés : jamais copiés. `.share-cache` contient les zips
/// du pack client, reconstruits à la demande.
const SKIP_ALWAYS: [&str; 4] = [".fabric", "logs", "crash-reports", ".share-cache"];
/// Régénérés au démarrage pour vanilla, Paper et Fabric seulement.
const SKIP_REGENERATED: [&str; 2] = ["libraries", "versions"];

fn skip_list(regenerates: bool) -> Vec<&'static str> {
    let mut v = SKIP_ALWAYS.to_vec();
    if regenerates {
        v.extend(SKIP_REGENERATED);
    }
    v
}
const SUFFIX: &str = ".tar.gz";

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "kebab-case")]
pub enum BackupKind {
    Auto,
    Manuel,
    AvantRestauration,
}

impl BackupKind {
    fn tag(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manuel => "manuel",
            Self::AvantRestauration => "avant-restauration",
        }
    }
    fn from_file(file: &str) -> Option<Self> {
        let stem = file.strip_suffix(SUFFIX)?;
        [Self::AvantRestauration, Self::Manuel, Self::Auto]
            .into_iter()
            .find(|k| stem.ends_with(&format!("-{}", k.tag())))
    }
    fn expires(self) -> bool {
        self != Self::Manuel
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct Backup {
    pub file: String,
    pub kind: BackupKind,
    pub size: u64,
    /// Secondes Unix (mtime de l'archive).
    pub created_at: u64,
}

/// Ce qui doit survivre à un redémarrage : qui a eu des joueurs depuis sa
/// dernière sauvegarde, et quel jour la tournée nocturne a eu lieu.
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct BackupState {
    #[serde(default)]
    pub activity: BTreeMap<String, bool>,
    #[serde(default)]
    pub last_run_day: Option<String>,
}

/// Sauvegardes à supprimer pour que `list` (de la plus récente à la plus
/// ancienne) tienne dans `max` octets : les plus anciennes d'abord, les
/// manuelles en dernier recours, jamais la plus récente.
fn quota_victims(list: &[Backup], max: u64) -> Vec<String> {
    let mut total: u64 = list.iter().map(|b| b.size).sum();
    let mut victims = vec![];
    for manual in [false, true] {
        for b in list.iter().skip(1).rev().filter(|b| (b.kind == BackupKind::Manuel) == manual) {
            if total <= max {
                return victims;
            }
            total -= b.size;
            victims.push(b.file.clone());
        }
    }
    victims
}

fn mtime_secs(m: &std::fs::Metadata) -> u64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Nom d'archive fourni par le client : un fichier `.tar.gz` d'un type connu.
fn check_file(file: &str) -> ApiResult<BackupKind> {
    if file.starts_with('.') || file.contains(['/', '\\']) || file.chars().any(char::is_control) {
        return Err(AppError::bad("nom de sauvegarde invalide"));
    }
    BackupKind::from_file(file).ok_or_else(|| AppError::bad("nom de sauvegarde invalide"))
}

fn check_id(id: &str) -> ApiResult<()> {
    if valid_id(id) {
        Ok(())
    } else {
        Err(AppError::bad("identifiant d'instance invalide"))
    }
}

/// Archive `src` (dossier d'instance) dans `out`, sans les dossiers de `skip`
/// à la racine. Un fichier présent sous plusieurs noms (liens physiques entre
/// `mods/` et le pack client `client/`) n'est stocké qu'une fois.
fn write_archive(src: &FsPath, out: &FsPath, skip: &[&str]) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let mut seen: std::collections::HashMap<(u64, u64), PathBuf> = Default::default();
    let f = std::fs::File::create(out).with_context(|| format!("création de {}", out.display()))?;
    let gz = flate2::write::GzEncoder::new(io::BufWriter::new(f), flate2::Compression::fast());
    let mut tar = tar::Builder::new(gz);
    tar.follow_symlinks(false);
    let mut stack = vec![src.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&dir)?.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            let rel = p.strip_prefix(src)?;
            let name = e.file_name();
            let name = name.to_string_lossy();
            if dir == src && skip.contains(&name.as_ref()) {
                continue;
            }
            if name == "session.lock" || name.starts_with(".instance.json") {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            let res = if ft.is_dir() {
                stack.push(p.clone());
                tar.append_dir(rel, &p)
            } else if ft.is_file() {
                match e.metadata() {
                    Ok(m) if m.nlink() > 1 => match seen.get(&(m.dev(), m.ino())) {
                        Some(first) => {
                            let mut h = tar::Header::new_gnu();
                            h.set_entry_type(tar::EntryType::Link);
                            h.set_size(0);
                            h.set_mode(m.mode());
                            h.set_mtime(m.mtime().max(0) as u64);
                            tar.append_link(&mut h, rel, first)
                        }
                        None => {
                            seen.insert((m.dev(), m.ino()), rel.to_path_buf());
                            tar.append_path_with_name(&p, rel)
                        }
                    },
                    _ => tar.append_path_with_name(&p, rel),
                }
            } else {
                tar.append_path_with_name(&p, rel)
            };
            match res {
                Ok(()) => {}
                // Fichier temporaire du serveur disparu entre la lecture du
                // dossier et sa copie : sans importance.
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("archivage de {}", p.display())),
            }
        }
    }
    let gz = tar.into_inner()?;
    let w = gz.finish()?;
    let f = w.into_inner().map_err(|e| e.into_error())?;
    f.sync_all()?;
    Ok(())
}

/// Extrait `archive` dans `dest` (qui ne doit pas exister).
fn extract_archive(archive: &FsPath, dest: &FsPath) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    let gz = flate2::read::GzDecoder::new(io::BufReader::new(std::fs::File::open(archive)?));
    let mut ar = tar::Archive::new(gz);
    ar.set_preserve_permissions(true);
    for entry in ar.entries()? {
        let mut entry = entry?;
        // `unpack_in` refuse les chemins qui sortiraient de `dest`.
        entry.unpack_in(dest)?;
    }
    Ok(())
}

impl App {
    pub fn backup_dir(&self) -> &FsPath {
        &self.cfg.backup_dir
    }

    fn instance_backup_dir(&self, id: &str) -> PathBuf {
        self.backup_dir().join(id)
    }

    fn state_path(&self) -> PathBuf {
        self.backup_dir().join("state.json")
    }

    pub fn load_backup_state(dir: &FsPath) -> BackupState {
        std::fs::read(dir.join("state.json"))
            .ok()
            .and_then(|r| serde_json::from_slice(&r).ok())
            .unwrap_or_default()
    }

    fn update_backup_state(&self, f: impl FnOnce(&mut BackupState) -> bool) {
        let mut s = self.backup_state.lock().unwrap();
        if !f(&mut s) {
            return;
        }
        match serde_json::to_vec_pretty(&*s) {
            Ok(data) => {
                if let Err(e) = write_atomic(&self.state_path(), &data) {
                    tracing::error!("{} : {e}", self.state_path().display());
                }
            }
            Err(e) => tracing::error!("état des sauvegardes : {e}"),
        }
    }

    fn mark_activity(&self, id: &str, active: bool) {
        self.update_backup_state(|s| {
            let cur = s.activity.get(id).copied().unwrap_or(false);
            if cur == active {
                return false;
            }
            if active {
                s.activity.insert(id.to_string(), true);
            } else {
                s.activity.remove(id);
            }
            true
        });
    }

    fn has_activity(&self, id: &str) -> bool {
        self.backup_state.lock().unwrap().activity.get(id).copied().unwrap_or(false)
    }

    pub fn list_backups(&self, id: &str) -> Vec<Backup> {
        let Ok(rd) = std::fs::read_dir(self.instance_backup_dir(id)) else { return vec![] };
        let mut out: Vec<Backup> = rd
            .flatten()
            .filter_map(|e| {
                let file = e.file_name().to_str()?.to_string();
                if file.starts_with('.') {
                    return None;
                }
                let kind = BackupKind::from_file(&file)?;
                let m = e.metadata().ok()?;
                m.is_file().then(|| Backup { file, kind, size: m.len(), created_at: mtime_secs(&m) })
            })
            .collect();
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.file.cmp(&a.file)));
        out
    }

    /// Identifiants qui ont des sauvegardes, instance encore présente ou non.
    fn backup_ids(&self) -> Vec<String> {
        let Ok(rd) = std::fs::read_dir(self.backup_dir()) else { return vec![] };
        let mut ids: Vec<String> = rd
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .filter(|id| valid_id(id))
            .collect();
        ids.sort();
        ids
    }

    /// Archive l'instance `id`. Si elle tourne, l'écriture du monde est
    /// suspendue le temps de la copie pour obtenir un état cohérent.
    pub async fn backup_instance(&self, id: &str, kind: BackupKind) -> ApiResult<Backup> {
        let inst = self.load_instance(id)?;
        if inst.status == InstallStatus::Installing {
            return Err(AppError::conflict("installation en cours"));
        }
        let _lock = self.backup_lock.lock().await;
        let dir = self.instance_backup_dir(id);
        tokio::fs::create_dir_all(&dir).await?;
        // Horodatage local, lisible et triable ; le suffixe évite toute collision.
        let mut name = format!("{}-{}{SUFFIX}", Local::now().format("%Y-%m-%d_%H%M%S"), kind.tag());
        let mut n = 2;
        while dir.join(&name).exists() {
            name = format!("{}-{n}-{}{SUFFIX}", Local::now().format("%Y-%m-%d_%H%M%S"), kind.tag());
            n += 1;
        }
        let out = dir.join(&name);
        let partial = dir.join(format!(".{name}.partial"));

        let live = self.server.running_instance().as_deref() == Some(id)
            && self.server.status().state == RunState::Running;
        self.server.log(format!("[omg] [{id}] sauvegarde {} en cours…", kind.tag()));
        if live {
            self.suspend_saves().await;
        }
        let (src, p) = (self.instance_dir(id), partial.clone());
        let skip = skip_list(inst.kind.regenerates_libraries());
        let res = tokio::task::spawn_blocking(move || write_archive(&src, &p, &skip)).await;
        if live {
            let _ = self.server.send_command("save-on").await;
        }
        if let Err(e) = res.map_err(anyhow::Error::from).and_then(|r| r) {
            let _ = tokio::fs::remove_file(&partial).await;
            self.server.log(format!("[omg] [{id}] sauvegarde échouée : {e:#}"));
            return Err(e.into());
        }
        tokio::fs::rename(&partial, &out).await?;
        // Des joueurs encore en ligne continuent de jouer : la prochaine
        // tournée devra repasser.
        let st = self.server.status();
        let players_online = st.instance.as_deref() == Some(id) && !st.players.is_empty();
        if !players_online {
            self.mark_activity(id, false);
        }
        self.enforce_quota(id);
        let m = tokio::fs::metadata(&out).await?;
        let b = Backup { file: name, kind, size: m.len(), created_at: mtime_secs(&m) };
        self.server.log(format!(
            "[omg] [{id}] sauvegarde {} : {} ({} Mo)",
            kind.tag(),
            b.file,
            b.size / (1024 * 1024)
        ));
        Ok(b)
    }

    /// `save-off` + `save-all flush`, puis attend que le serveur confirme
    /// (« Saved the game », ou « Saved the world » sur les vieilles versions).
    async fn suspend_saves(&self) {
        let (_, mut logs) = self.server.subscribe_logs();
        let _ = self.server.send_command("save-off").await;
        let _ = self.server.send_command("save-all flush").await;
        let confirmed = tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                match logs.recv().await {
                    Ok(l) if l.contains("Saved the game") || l.contains("Saved the world") => return,
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => return,
                }
            }
        })
        .await
        .is_ok();
        if !confirmed {
            self.server.log("[omg] pas de confirmation de « save-all », sauvegarde quand même");
        }
    }

    /// Remplace l'instance par le contenu de l'archive. L'état actuel est
    /// d'abord sauvegardé ; `libraries/`, `versions/`, `.fabric/` et `logs/`
    /// (absents des archives) sont repris de l'instance actuelle.
    pub async fn restore_backup(&self, id: &str, file: &str) -> ApiResult<()> {
        check_id(id)?;
        check_file(file)?;
        let archive = self.instance_backup_dir(id).join(file);
        if !archive.is_file() {
            return Err(AppError::not_found("sauvegarde introuvable"));
        }
        let _op = self.op.lock().await;
        if self.server.running_instance().as_deref() == Some(id) {
            return Err(AppError::conflict("arrête d'abord le serveur"));
        }
        let current = self.instance_dir(id);
        if current.join("instance.json").exists() {
            self.backup_instance(id, BackupKind::AvantRestauration).await?;
        }
        self.server.log(format!("[omg] [{id}] restauration de {file}…"));
        let staging = self.tmp_path("restore");
        let old = self.tmp_path("old");
        let (a, s, c, o) = (archive.clone(), staging.clone(), current.clone(), old.clone());
        let res = tokio::task::spawn_blocking(move || -> Result<()> {
            extract_archive(&a, &s)?;
            anyhow::ensure!(s.join("instance.json").is_file(), "archive sans instance.json");
            if c.exists() {
                for keep in SKIP_ALWAYS.iter().chain(&SKIP_REGENERATED) {
                    let (from, to) = (c.join(keep), s.join(keep));
                    if from.exists() && !to.exists() {
                        std::fs::rename(&from, &to)?;
                    }
                }
                std::fs::rename(&c, &o)?;
            }
            std::fs::rename(&s, &c)?;
            Ok(())
        })
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
        if let Err(e) = res {
            let _ = std::fs::remove_dir_all(&staging);
            // L'ancien dossier a pu être mis de côté sans que le nouveau prenne sa place.
            if !current.exists() && old.exists() {
                let _ = std::fs::rename(&old, &current);
            }
            self.server.log(format!("[omg] [{id}] restauration échouée : {e:#}"));
            return Err(e.into());
        }
        tokio::task::spawn_blocking(move || std::fs::remove_dir_all(old));
        // Une instance restaurée n'est jamais « en cours d'installation ».
        let _ = self.update_instance(id, |i| {
            if i.status == InstallStatus::Installing {
                i.status = InstallStatus::Ready;
            }
        });
        self.server.log(format!("[omg] [{id}] restaurée depuis {file}"));
        Ok(())
    }

    /// Plafond de place par instance (`backup_max_bytes`).
    fn enforce_quota(&self, id: &str) {
        let list = self.list_backups(id);
        for file in quota_victims(&list, self.cfg.backup_max_bytes) {
            match std::fs::remove_file(self.instance_backup_dir(id).join(&file)) {
                Ok(()) => self.server.log(format!("[omg] [{id}] place limitée, sauvegarde supprimée : {file}")),
                Err(e) => tracing::warn!("{id}/{file} : {e}"),
            }
        }
    }

    /// Supprime les sauvegardes expirées ; garde toujours l'auto la plus récente.
    fn prune_backups(&self) {
        let limit = crate::instances::now().saturating_sub(self.cfg.backup_retention_days * 86_400);
        for id in self.backup_ids() {
            let list = self.list_backups(&id);
            let newest_auto = list.iter().find(|b| b.kind == BackupKind::Auto).map(|b| b.file.clone());
            for b in list {
                if !b.kind.expires() || b.created_at >= limit || Some(&b.file) == newest_auto.as_ref() {
                    continue;
                }
                match std::fs::remove_file(self.instance_backup_dir(&id).join(&b.file)) {
                    Ok(()) => self.server.log(format!("[omg] [{id}] sauvegarde expirée supprimée : {}", b.file)),
                    Err(e) => tracing::warn!("{id}/{} : {e}", b.file),
                }
            }
            self.enforce_quota(&id);
        }
    }

    /// Tournée nocturne : une sauvegarde par instance active depuis la veille.
    async fn nightly(&self) {
        let ids: Vec<String> = {
            let s = self.backup_state.lock().unwrap();
            s.activity.iter().filter(|(_, v)| **v).map(|(k, _)| k.clone()).collect()
        };
        if ids.is_empty() {
            self.server.log("[omg] sauvegarde auto : personne n'a joué, rien à faire");
        }
        for id in ids {
            match self.backup_instance(&id, BackupKind::Auto).await {
                Ok(_) => {}
                // Instance supprimée entre-temps : on l'oublie.
                Err(AppError(StatusCode::NOT_FOUND, _)) => self.mark_activity(&id, false),
                Err(e) => tracing::error!("sauvegarde auto de {id} : {}", e.1),
            }
        }
        self.prune_backups();
    }

    /// Tâches de fond : suivi de l'activité des joueurs et tournée nocturne.
    pub fn spawn_backups(self: &Arc<Self>) {
        // Nettoyage des archives à moitié écrites par un arrêt brutal.
        for id in self.backup_ids() {
            if let Ok(rd) = std::fs::read_dir(self.instance_backup_dir(&id)) {
                for e in rd.flatten() {
                    if e.file_name().to_string_lossy().ends_with(".partial") {
                        let _ = std::fs::remove_file(e.path());
                    }
                }
            }
        }

        let app = self.clone();
        tokio::spawn(async move {
            let mut st = app.server.watch_status();
            loop {
                let (inst, has_players) = {
                    let s = st.borrow_and_update();
                    (s.instance.clone(), !s.players.is_empty())
                };
                if let (Some(id), true) = (inst, has_players) {
                    if !app.has_activity(&id) {
                        app.mark_activity(&id, true);
                    }
                }
                tokio::select! {
                    _ = app.shutdown.cancelled() => return,
                    r = st.changed() => if r.is_err() { return },
                }
            }
        });

        let app = self.clone();
        tokio::spawn(async move {
            loop {
                let now = Local::now();
                let today = now.format("%Y-%m-%d").to_string();
                let due = now.hour() >= app.cfg.backup_hour
                    && app.backup_state.lock().unwrap().last_run_day.as_deref() != Some(today.as_str());
                if due {
                    app.nightly().await;
                    app.update_backup_state(|s| {
                        s.last_run_day = Some(today);
                        true
                    });
                }
                tokio::select! {
                    _ = app.shutdown.cancelled() => return,
                    _ = tokio::time::sleep(Duration::from_secs(60)) => {}
                }
            }
        });
    }
}

// --- API ---------------------------------------------------------------------

/// Toutes les sauvegardes, regroupées par instance — y compris celles d'une
/// instance supprimée, pour pouvoir la faire revenir.
pub async fn list_all(State(app): AppState) -> ApiResult<Json<Value>> {
    let existing: Vec<String> = app.list_instances()?.into_iter().map(|i| i.id).collect();
    let mut ids = app.backup_ids();
    for id in &existing {
        if !ids.contains(id) {
            ids.push(id.clone());
        }
    }
    ids.sort();
    let groups: Vec<Value> = ids
        .iter()
        .map(|id| {
            json!({
                "instance": id,
                "exists": existing.contains(id),
                "activity": app.has_activity(id),
                "backups": app.list_backups(id),
            })
        })
        .collect();
    Ok(Json(json!({
        "hour": app.cfg.backup_hour,
        "retention_days": app.cfg.backup_retention_days,
        "max_bytes": app.cfg.backup_max_bytes,
        "groups": groups,
    })))
}

pub async fn create(State(app): AppState, Path(id): Path<String>) -> ApiResult<Json<Backup>> {
    Ok(Json(app.backup_instance(&id, BackupKind::Manuel).await?))
}

pub async fn restore(
    State(app): AppState,
    Path((id, file)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    app.restore_backup(&id, &file).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn remove(
    State(app): AppState,
    Path((id, file)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    check_id(&id)?;
    check_file(&file)?;
    let p = app.instance_backup_dir(&id).join(&file);
    tokio::fs::remove_file(&p).await.map_err(|_| AppError::not_found("sauvegarde introuvable"))?;
    app.server.log(format!("[omg] [{id}] sauvegarde supprimée : {file}"));
    Ok(StatusCode::NO_CONTENT)
}

pub async fn download(
    State(app): AppState,
    Path((id, file)): Path<(String, String)>,
) -> ApiResult<Response> {
    check_id(&id)?;
    check_file(&file)?;
    let f = tokio::fs::File::open(app.instance_backup_dir(&id).join(&file))
        .await
        .map_err(|_| AppError::not_found("sauvegarde introuvable"))?;
    let len = f.metadata().await?.len();
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "application/gzip")
        .header(header::CONTENT_LENGTH, len)
        .header(header::CONTENT_DISPOSITION, format!("attachment; filename=\"{id}-{file}\""))
        .body(Body::from_stream(ReaderStream::new(f)))
        .map_err(anyhow::Error::from)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_from_names() {
        assert_eq!(BackupKind::from_file("2026-11-21_040000-auto.tar.gz"), Some(BackupKind::Auto));
        assert_eq!(BackupKind::from_file("2026-11-21_040000-2-manuel.tar.gz"), Some(BackupKind::Manuel));
        assert_eq!(
            BackupKind::from_file("2026-11-21_040000-avant-restauration.tar.gz"),
            Some(BackupKind::AvantRestauration)
        );
        assert_eq!(BackupKind::from_file("x.zip"), None);
        assert!(check_file("../x-auto.tar.gz").is_err());
        assert!(check_file(".a-auto.tar.gz").is_err());
        assert!(check_file("a-auto.tar.gz").is_ok());
    }

    #[test]
    fn quota_keeps_newest_and_manual_last() {
        let b = |file: &str, kind, size| Backup { file: file.into(), kind, size, created_at: 0 };
        // De la plus récente à la plus ancienne.
        let list = vec![
            b("5", BackupKind::Manuel, 10),
            b("4", BackupKind::Auto, 10),
            b("3", BackupKind::Manuel, 10),
            b("2", BackupKind::AvantRestauration, 10),
            b("1", BackupKind::Auto, 10),
        ];
        assert!(quota_victims(&list, 50).is_empty());
        assert_eq!(quota_victims(&list, 35), vec!["1", "2"]);
        assert_eq!(quota_victims(&list, 20), vec!["1", "2", "4"]);
        assert_eq!(quota_victims(&list, 5), vec!["1", "2", "4", "3"]);
        // Une seule sauvegarde plus grosse que le plafond : on la garde.
        assert!(quota_victims(&[b("x", BackupKind::Auto, 99)], 5).is_empty());
    }

    #[test]
    fn archive_roundtrip_skips_regenerated() {
        let base = std::env::temp_dir().join(format!("omg-bk-{}", std::process::id()));
        let src = base.join("src");
        std::fs::create_dir_all(src.join("world/region")).unwrap();
        std::fs::create_dir_all(src.join("libraries/x")).unwrap();
        std::fs::write(src.join("instance.json"), "{}").unwrap();
        std::fs::write(src.join("world/level.dat"), "lvl").unwrap();
        std::fs::write(src.join("world/session.lock"), "lock").unwrap();
        std::fs::write(src.join("world/region/r.0.0.mca"), vec![7u8; 10_000]).unwrap();
        std::fs::write(src.join("libraries/x/a.jar"), "jar").unwrap();
        let out = base.join("b.tar.gz");
        std::fs::create_dir_all(src.join("client/mods")).unwrap();
        std::fs::write(src.join("mods.jar"), vec![1u8; 5000]).unwrap();
        std::fs::hard_link(src.join("mods.jar"), src.join("client/mods/mods.jar")).unwrap();
        write_archive(&src, &out, &skip_list(true)).unwrap();
        let dest = base.join("dest");
        extract_archive(&out, &dest).unwrap();
        assert_eq!(std::fs::read(dest.join("world/level.dat")).unwrap(), b"lvl");
        assert_eq!(std::fs::read(dest.join("world/region/r.0.0.mca")).unwrap().len(), 10_000);
        assert!(dest.join("instance.json").is_file());
        assert!(!dest.join("libraries").exists());
        assert!(!dest.join("world/session.lock").exists());
        assert_eq!(std::fs::read(dest.join("client/mods/mods.jar")).unwrap().len(), 5000);
        assert_eq!(std::fs::read(dest.join("mods.jar")).unwrap().len(), 5000);
        std::fs::remove_dir_all(base).unwrap();
    }
}
