//! Explorateur de fichiers d'une instance : l'onglet « Fichiers » est un petit
//! éditeur de texte, toute la configuration passe par lui.

use std::{
    path::{Component, Path as FsPath, PathBuf},
    sync::Arc,
    time::UNIX_EPOCH,
};

use axum::{
    body::Body,
    extract::{Multipart, Path, Query, State},
    http::{header, StatusCode},
    response::Response,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

use crate::{
    app::App,
    error::{ApiResult, AppError},
    instances::{safe_name, write_atomic, Instance},
};

type AppState = State<Arc<App>>;

/// Listes d'accès de Minecraft : sans objet ici, c'est NetBird qui filtre.
const HIDDEN: [&str; 3] = ["whitelist.json", "banned-players.json", "banned-ips.json"];

/// Au-delà, l'éditeur refuse : le fichier se télécharge.
const MAX_EDIT: u64 = 2 * 1024 * 1024;

#[derive(Deserialize)]
pub struct PathQuery {
    #[serde(default)]
    path: String,
}

/// Chemin relatif fourni par le client → chemin absolu dans l'instance.
/// Refuse `..`, les chemins absolus et les liens symboliques qui sortent.
fn resolve(app: &App, id: &str, rel: &str) -> ApiResult<PathBuf> {
    app.load_instance(id)?;
    let base = app.instance_dir(id);
    let mut p = base.clone();
    for c in FsPath::new(rel.trim_matches('/')).components() {
        match c {
            Component::Normal(s) => p.push(s),
            Component::CurDir => {}
            _ => return Err(AppError::bad(format!("chemin invalide : {rel}"))),
        }
    }
    let base_c = base.canonicalize()?;
    let mut probe = p.as_path();
    while !probe.exists() {
        probe = probe.parent().unwrap_or(&base);
    }
    if !probe.canonicalize()?.starts_with(&base_c) {
        return Err(AppError::bad("chemin hors de l'instance"));
    }
    Ok(p)
}

fn rel_of(app: &App, id: &str, p: &FsPath) -> String {
    p.strip_prefix(app.instance_dir(id))
        .map(|r| r.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn mtime(m: &std::fs::Metadata) -> u64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Ce que le serveur lit en ce moment : on n'y supprime ni ne renomme rien.
fn guard_live(app: &App, id: &str, p: &FsPath) -> ApiResult<()> {
    if let Some(w) = app.server.live_world(id) {
        if p.starts_with(app.instance_dir(id).join(w)) {
            return Err(AppError::conflict("ce monde est en cours d'utilisation"));
        }
    }
    Ok(())
}

fn guard_protected(app: &App, id: &str, p: &FsPath) -> ApiResult<()> {
    if p == app.instance_dir(id) || p == app.instance_dir(id).join("instance.json") {
        return Err(AppError::bad("élément protégé"));
    }
    Ok(())
}

#[derive(Serialize)]
struct Entry {
    name: String,
    dir: bool,
    size: u64,
    mtime: u64,
}

pub async fn list(
    State(app): AppState,
    Path(id): Path<String>,
    Query(q): Query<PathQuery>,
) -> ApiResult<Json<Value>> {
    let dir = resolve(&app, &id, &q.path)?;
    let rd = std::fs::read_dir(&dir).map_err(|_| AppError::not_found("dossier introuvable"))?;
    let mut entries = vec![];
    for e in rd.flatten() {
        let Some(name) = e.file_name().to_str().map(str::to_string) else { continue };
        let Ok(m) = e.metadata() else { continue };
        if name.starts_with(".instance.json") || HIDDEN.contains(&name.as_str()) {
            continue;
        }
        entries.push(Entry { name, dir: m.is_dir(), size: m.len(), mtime: mtime(&m) });
    }
    entries.sort_by(|a, b| b.dir.cmp(&a.dir).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(Json(json!({ "entries": entries })))
}

pub async fn read(
    State(app): AppState,
    Path(id): Path<String>,
    Query(q): Query<PathQuery>,
) -> ApiResult<Json<Value>> {
    let p = resolve(&app, &id, &q.path)?;
    let m = std::fs::metadata(&p).map_err(|_| AppError::not_found("fichier introuvable"))?;
    if m.is_dir() {
        return Err(AppError::bad("c'est un dossier"));
    }
    if m.len() > MAX_EDIT {
        return Err(AppError(
            StatusCode::PAYLOAD_TOO_LARGE,
            "fichier trop gros pour l'éditeur (2 Mo max) — téléchargez-le".into(),
        ));
    }
    let raw = tokio::fs::read(&p).await?;
    if raw.contains(&0) {
        return Err(AppError(StatusCode::UNSUPPORTED_MEDIA_TYPE, "fichier binaire".into()));
    }
    let content = String::from_utf8(raw)
        .map_err(|_| AppError(StatusCode::UNSUPPORTED_MEDIA_TYPE, "fichier binaire ou non UTF-8".into()))?;
    Ok(Json(json!({ "content": content, "mtime": mtime(&m) })))
}

#[derive(Deserialize)]
pub struct WriteReq {
    path: String,
    content: String,
    /// mtime lu à l'ouverture : si le fichier a changé depuis (le serveur
    /// réécrit `ops.json`, `usercache.json`…), on refuse d'écraser.
    mtime: Option<u64>,
    #[serde(default)]
    create: bool,
}

pub async fn write(
    State(app): AppState,
    Path(id): Path<String>,
    Json(req): Json<WriteReq>,
) -> ApiResult<Json<Value>> {
    let p = resolve(&app, &id, &req.path)?;
    if p == app.instance_dir(&id) {
        return Err(AppError::bad("chemin invalide"));
    }
    if req.create {
        safe_name(p.file_name().and_then(|n| n.to_str()).unwrap_or(""))?;
    }
    match std::fs::metadata(&p) {
        Ok(m) if m.is_dir() => return Err(AppError::bad("c'est un dossier")),
        Ok(_) if req.create => return Err(AppError::conflict("ce fichier existe déjà")),
        Ok(m) => {
            if let Some(t) = req.mtime {
                if mtime(&m) != t {
                    return Err(AppError::conflict(
                        "le fichier a été modifié entre-temps (par le serveur ?) — rechargez-le",
                    ));
                }
            }
        }
        Err(_) if !req.create => return Err(AppError::not_found("fichier introuvable")),
        Err(_) => {}
    }
    if p == app.instance_dir(&id).join("instance.json") {
        validate_instance_json(&req.content)?;
    }
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_atomic(&p, req.content.as_bytes())?;
    let m = std::fs::metadata(&p)?;
    Ok(Json(json!({ "mtime": mtime(&m) })))
}

/// `instance.json` est éditable comme le reste, mais un fichier invalide
/// rendrait l'instance invisible : on le vérifie avant d'écrire.
fn validate_instance_json(content: &str) -> ApiResult<()> {
    let i: Instance =
        serde_json::from_str(content).map_err(|e| AppError::bad(format!("instance.json invalide : {e}")))?;
    if !(512..=65536).contains(&i.memory_max_mb) || i.memory_min_mb > i.memory_max_mb {
        return Err(AppError::bad("instance.json : mémoire entre 512 et 65536 Mo, min ≤ max"));
    }
    if i.java_major != 0 && !(8..=99).contains(&i.java_major) {
        return Err(AppError::bad("instance.json : java_major invalide"));
    }
    safe_name(&i.jar).map_err(|_| AppError::bad("instance.json : jar invalide"))?;
    if i.name.trim().is_empty() {
        return Err(AppError::bad("instance.json : name requis"));
    }
    Ok(())
}

pub async fn mkdir(
    State(app): AppState,
    Path(id): Path<String>,
    Json(q): Json<PathQuery>,
) -> ApiResult<StatusCode> {
    let p = resolve(&app, &id, &q.path)?;
    safe_name(p.file_name().and_then(|n| n.to_str()).unwrap_or(""))?;
    if p.exists() {
        return Err(AppError::conflict("existe déjà"));
    }
    std::fs::create_dir_all(&p)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct RenameReq {
    from: String,
    to: String,
}

pub async fn rename(
    State(app): AppState,
    Path(id): Path<String>,
    Json(req): Json<RenameReq>,
) -> ApiResult<StatusCode> {
    let from = resolve(&app, &id, &req.from)?;
    let to = resolve(&app, &id, &req.to)?;
    guard_protected(&app, &id, &from)?;
    guard_protected(&app, &id, &to)?;
    guard_live(&app, &id, &from)?;
    safe_name(to.file_name().and_then(|n| n.to_str()).unwrap_or(""))?;
    if !from.exists() {
        return Err(AppError::not_found("introuvable"));
    }
    if to.exists() {
        return Err(AppError::conflict("la destination existe déjà"));
    }
    if to.starts_with(&from) {
        return Err(AppError::bad("impossible de déplacer un dossier dans lui-même"));
    }
    std::fs::rename(&from, &to)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn remove(
    State(app): AppState,
    Path(id): Path<String>,
    Query(q): Query<PathQuery>,
) -> ApiResult<StatusCode> {
    let p = resolve(&app, &id, &q.path)?;
    guard_protected(&app, &id, &p)?;
    guard_live(&app, &id, &p)?;
    let m = std::fs::symlink_metadata(&p).map_err(|_| AppError::not_found("introuvable"))?;
    let rel = rel_of(&app, &id, &p);
    if m.is_dir() {
        tokio::task::spawn_blocking(move || std::fs::remove_dir_all(p)).await??;
    } else {
        tokio::fs::remove_file(&p).await?;
    }
    app.server.log(format!("[omg] [{id}] supprimé : {rel}"));
    Ok(StatusCode::NO_CONTENT)
}

pub async fn download(
    State(app): AppState,
    Path(id): Path<String>,
    Query(q): Query<PathQuery>,
) -> ApiResult<Response> {
    let p = resolve(&app, &id, &q.path)?;
    let file = tokio::fs::File::open(&p).await.map_err(|_| AppError::not_found("introuvable"))?;
    let m = file.metadata().await?;
    if m.is_dir() {
        return Err(AppError::bad("c'est un dossier"));
    }
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("fichier").replace('"', "");
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, m.len())
        .header(header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\""))
        .body(Body::from_stream(ReaderStream::new(file)))
        .map_err(anyhow::Error::from)?)
}

/// Envoi de fichiers dans un dossier (`?path=`), en écrasant les homonymes.
pub async fn upload(
    State(app): AppState,
    Path(id): Path<String>,
    Query(q): Query<PathQuery>,
    mut mp: Multipart,
) -> ApiResult<Json<Value>> {
    let dir = resolve(&app, &id, &q.path)?;
    if !dir.is_dir() {
        return Err(AppError::not_found("dossier introuvable"));
    }
    let mut added = vec![];
    while let Some(mut field) = mp.next_field().await? {
        let Some(name) = field.file_name().map(str::to_string) else { continue };
        safe_name(&name)?;
        let dest = dir.join(&name);
        guard_protected(&app, &id, &dest)?;
        let tmp = app.tmp_path("upload");
        let mut f = tokio::fs::File::create(&tmp).await?;
        while let Some(chunk) = field.chunk().await? {
            f.write_all(&chunk).await?;
        }
        f.flush().await?;
        drop(f);
        tokio::fs::rename(&tmp, &dest).await?;
        added.push(name);
    }
    Ok(Json(json!({ "added": added })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_json_validation() {
        let ok = r#"{"name":"A","kind":"vanilla","mc_version":"1.20.1","jar":"server.jar",
            "java_major":17,"memory_min_mb":1024,"memory_max_mb":2048}"#;
        assert!(validate_instance_json(ok).is_ok());
        assert!(validate_instance_json(&ok.replace("server.jar", "../x.jar")).is_err());
        assert!(validate_instance_json(&ok.replace("2048", "100")).is_err());
        assert!(validate_instance_json("{").is_err());
    }
}
