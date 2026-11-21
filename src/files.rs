//! Mods (`mods/*.jar`) et mondes (dossiers contenant un `level.dat`).

use std::{
    io::{Read, Write},
    path::{Path as FsPath, PathBuf},
    sync::Arc,
    time::Duration,
};

use axum::{
    body::Body,
    extract::{multipart::Field, Multipart, Path, State},
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
    instances::safe_name,
    properties::Properties,
};

type AppState = State<Arc<App>>;

async fn save_field(mut field: Field<'_>, dest: &FsPath) -> ApiResult<()> {
    let mut f = tokio::fs::File::create(dest).await?;
    while let Some(chunk) = field.chunk().await? {
        f.write_all(&chunk).await?;
    }
    f.flush().await?;
    Ok(())
}

fn dir_size(p: &FsPath) -> u64 {
    let Ok(rd) = std::fs::read_dir(p) else { return 0 };
    rd.flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(_) => e.metadata().map(|m| m.len()).unwrap_or(0),
            Err(_) => 0,
        })
        .sum()
}

// --- mods --------------------------------------------------------------------

#[derive(Serialize)]
struct ModFile {
    file: String,
    enabled: bool,
    size: u64,
}

pub async fn list_mods(State(app): AppState, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    app.load_instance(&id)?;
    let dir = app.instance_dir(&id).join("mods");
    let mut mods = vec![];
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let Some(file) = e.file_name().to_str().map(str::to_string) else { continue };
            let enabled = file.ends_with(".jar");
            if !enabled && !file.ends_with(".jar.disabled") {
                continue;
            }
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            mods.push(ModFile { file, enabled, size });
        }
    }
    mods.sort_by(|a, b| a.file.to_lowercase().cmp(&b.file.to_lowercase()));
    Ok(Json(json!({ "mods": mods })))
}

pub async fn upload_mods(
    State(app): AppState,
    Path(id): Path<String>,
    mut mp: Multipart,
) -> ApiResult<Json<Value>> {
    app.load_instance(&id)?;
    let dir = app.instance_dir(&id).join("mods");
    tokio::fs::create_dir_all(&dir).await?;
    let mut added = vec![];
    while let Some(field) = mp.next_field().await? {
        let Some(name) = field.file_name().map(str::to_string) else { continue };
        safe_name(&name)?;
        if !name.ends_with(".jar") {
            return Err(AppError::bad(format!("{name} : seuls les .jar sont acceptés")));
        }
        let tmp = app.tmp_path("jar");
        save_field(field, &tmp).await?;
        tokio::fs::rename(&tmp, dir.join(&name)).await?;
        let _ = tokio::fs::remove_file(dir.join(format!("{name}.disabled"))).await;
        added.push(name);
    }
    app.server.log(format!("[omg] [{id}] mods ajoutés : {}", added.join(", ")));
    // Un fichier attendu par le modpack va aussi dans le pack client.
    app.reconcile_missing(&id);
    Ok(Json(json!({ "added": added })))
}

pub async fn delete_mod(
    State(app): AppState,
    Path((id, file)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    app.load_instance(&id)?;
    safe_name(&file)?;
    if !file.ends_with(".jar") && !file.ends_with(".jar.disabled") {
        return Err(AppError::bad("pas un mod"));
    }
    tokio::fs::remove_file(app.instance_dir(&id).join("mods").join(&file))
        .await
        .map_err(|_| AppError::not_found("mod introuvable"))?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn toggle_mod(
    State(app): AppState,
    Path((id, file)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    app.load_instance(&id)?;
    safe_name(&file)?;
    let dir = app.instance_dir(&id).join("mods");
    let target = if let Some(base) = file.strip_suffix(".disabled") {
        base.to_string()
    } else if file.ends_with(".jar") {
        format!("{file}.disabled")
    } else {
        return Err(AppError::bad("pas un mod"));
    };
    tokio::fs::rename(dir.join(&file), dir.join(target))
        .await
        .map_err(|_| AppError::not_found("mod introuvable"))?;
    Ok(StatusCode::NO_CONTENT)
}

// --- mondes ------------------------------------------------------------------

fn level_name(app: &App, id: &str) -> ApiResult<String> {
    let p = Properties::load(&app.instance_dir(id).join("server.properties"))?;
    Ok(p.get("level-name").filter(|s| !s.is_empty()).unwrap_or("world").to_string())
}

fn set_props(app: &App, id: &str, kv: &[(&str, &str)]) -> ApiResult<()> {
    let path = app.instance_dir(id).join("server.properties");
    let mut p = Properties::load(&path)?;
    for (k, v) in kv {
        p.set(k, v);
    }
    p.save(&path)?;
    Ok(())
}

#[derive(Serialize)]
struct World {
    name: String,
    size: u64,
}

pub async fn list_worlds(State(app): AppState, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    app.load_instance(&id)?;
    let dir = app.instance_dir(&id);
    let active = level_name(&app, &id)?;
    let worlds = tokio::task::spawn_blocking(move || {
        let mut out = vec![];
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if !p.join("level.dat").is_file() {
                continue;
            }
            let Some(name) = e.file_name().to_str().map(str::to_string) else { continue };
            out.push(World { size: dir_size(&p), name });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    })
    .await?;
    Ok(Json(json!({ "worlds": worlds, "active": active })))
}

#[derive(Deserialize)]
pub struct CreateWorld {
    name: String,
    #[serde(default)]
    seed: String,
}

/// Un nouveau monde = un `level-name` qui n'existe pas encore : le serveur le
/// génère au prochain démarrage.
pub async fn create_world(
    State(app): AppState,
    Path(id): Path<String>,
    Json(req): Json<CreateWorld>,
) -> ApiResult<StatusCode> {
    app.load_instance(&id)?;
    let name = safe_name(req.name.trim())?;
    if req.seed.chars().any(char::is_control) {
        return Err(AppError::bad("graine invalide"));
    }
    if app.instance_dir(&id).join(name).exists() {
        return Err(AppError::conflict("ce nom existe déjà"));
    }
    set_props(&app, &id, &[("level-name", name), ("level-seed", req.seed.trim())])?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn activate_world(
    State(app): AppState,
    Path((id, name)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    app.load_instance(&id)?;
    safe_name(&name)?;
    if !app.instance_dir(&id).join(&name).join("level.dat").is_file() {
        return Err(AppError::not_found("monde introuvable"));
    }
    set_props(&app, &id, &[("level-name", &name)])?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_world(
    State(app): AppState,
    Path((id, name)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    app.load_instance(&id)?;
    safe_name(&name)?;
    if app.server.live_world(&id).as_deref() == Some(name.as_str()) {
        return Err(AppError::conflict("ce monde est en cours d'utilisation"));
    }
    let dir = app.instance_dir(&id).join(&name);
    if !dir.join("level.dat").is_file() {
        return Err(AppError::not_found("monde introuvable"));
    }
    tokio::task::spawn_blocking(move || std::fs::remove_dir_all(dir)).await??;
    app.server.log(format!("[omg] [{id}] monde « {name} » supprimé"));
    Ok(StatusCode::NO_CONTENT)
}

/// Import d'une map en `.zip` : `level.dat` peut être à la racine de l'archive
/// ou dans un sous-dossier, c'est son dossier qui devient le monde.
pub async fn upload_world(
    State(app): AppState,
    Path(id): Path<String>,
    mut mp: Multipart,
) -> ApiResult<Json<Value>> {
    app.load_instance(&id)?;
    let mut zip_path: Option<PathBuf> = None;
    let mut zip_stem = None;
    let mut name = None;
    while let Some(field) = mp.next_field().await? {
        match field.name() {
            Some("name") => name = Some(field.text().await?.trim().to_string()),
            Some("file") => {
                zip_stem = field
                    .file_name()
                    .map(|f| f.trim_end_matches(".zip").to_string());
                let tmp = app.tmp_path("zip");
                save_field(field, &tmp).await?;
                zip_path = Some(tmp);
            }
            _ => {}
        }
    }
    let zip_path = zip_path.ok_or_else(|| AppError::bad("fichier .zip manquant"))?;
    let staging = app.tmp_path("world");
    let (z, s) = (zip_path.clone(), staging.clone());
    let res = tokio::task::spawn_blocking(move || extract_world(&z, &s)).await?;
    let _ = tokio::fs::remove_file(&zip_path).await;
    let root_name = match res {
        Ok(n) => n,
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            return Err(AppError::bad(format!("{e:#}")));
        }
    };
    let name = name
        .filter(|n| !n.is_empty())
        .or(root_name)
        .or(zip_stem)
        .unwrap_or_else(|| "world-importe".into());
    let cleanup = || async { let _ = tokio::fs::remove_dir_all(&staging).await; };
    if let Err(e) = safe_name(&name) {
        cleanup().await;
        return Err(e);
    }
    let dest = app.instance_dir(&id).join(&name);
    if dest.exists() {
        cleanup().await;
        return Err(AppError::conflict(format!("un dossier « {name} » existe déjà")));
    }
    tokio::fs::rename(&staging, &dest).await?;
    app.server.log(format!("[omg] [{id}] monde « {name} » importé"));
    Ok(Json(json!({ "name": name })))
}

/// Extrait le monde contenu dans `zip` vers `dest` ; renvoie le nom du dossier
/// qui contenait `level.dat` dans l'archive, s'il y en avait un.
fn extract_world(zip: &FsPath, dest: &FsPath) -> anyhow::Result<Option<String>> {
    let mut ar = zip::ZipArchive::new(std::fs::File::open(zip)?)?;
    let mut root: Option<PathBuf> = None;
    for i in 0..ar.len() {
        let f = ar.by_index(i)?;
        let Some(p) = f.enclosed_name() else { continue };
        if p.file_name().is_some_and(|n| n == "level.dat")
            && root.as_ref().is_none_or(|r| p.components().count() < r.components().count() + 1)
        {
            root = Some(p.parent().map(PathBuf::from).unwrap_or_default());
        }
    }
    let root = root.ok_or_else(|| anyhow::anyhow!("aucun level.dat dans l'archive"))?;
    std::fs::create_dir_all(dest)?;
    for i in 0..ar.len() {
        let mut f = ar.by_index(i)?;
        // Les liens symboliques d'une archive ne sont jamais recréés.
        if f.is_symlink() {
            continue;
        }
        let Some(p) = f.enclosed_name() else { continue };
        let Ok(rel) = p.strip_prefix(&root) else { continue };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let out = dest.join(rel);
        if f.is_dir() {
            std::fs::create_dir_all(&out)?;
        } else {
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut w = std::fs::File::create(&out)?;
            std::io::copy(&mut f, &mut w)?;
        }
    }
    Ok(root.file_name().and_then(|n| n.to_str()).map(str::to_string))
}

fn zip_dir(src: &FsPath, out: &FsPath, prefix: &str) -> anyhow::Result<()> {
    let mut w = zip::ZipWriter::new(std::io::BufWriter::new(std::fs::File::create(out)?));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);
    let mut stack = vec![src.to_path_buf()];
    let mut buf = vec![0u8; 1 << 16];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir)?.flatten() {
            let p = e.path();
            let rel = p.strip_prefix(src)?.to_string_lossy().replace('\\', "/");
            let ft = e.file_type()?;
            if ft.is_dir() {
                w.add_directory(format!("{prefix}/{rel}/"), opts)?;
                stack.push(p);
            } else if ft.is_file() {
                if rel == "session.lock" {
                    continue;
                }
                w.start_file(format!("{prefix}/{rel}"), opts)?;
                let mut f = std::fs::File::open(&p)?;
                loop {
                    let n = f.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    w.write_all(&buf[..n])?;
                }
            }
        }
    }
    w.finish()?.flush()?;
    Ok(())
}

/// Sauvegarde téléchargeable. Si le monde tourne, on suspend l'écriture le
/// temps de l'archiver pour obtenir une copie cohérente.
pub async fn download_world(
    State(app): AppState,
    Path((id, name)): Path<(String, String)>,
) -> ApiResult<Response> {
    app.load_instance(&id)?;
    safe_name(&name)?;
    let dir = app.instance_dir(&id).join(&name);
    if !dir.join("level.dat").is_file() {
        return Err(AppError::not_found("monde introuvable"));
    }
    let live = app.server.live_world(&id).as_deref() == Some(name.as_str());
    if live {
        let _ = app.server.send_command("save-off").await;
        let _ = app.server.send_command("save-all flush").await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    let out = app.tmp_path("zip");
    let (d, o, n) = (dir.clone(), out.clone(), name.clone());
    let res = tokio::task::spawn_blocking(move || zip_dir(&d, &o, &n)).await;
    if live {
        let _ = app.server.send_command("save-on").await;
    }
    if let Err(e) = res.map_err(anyhow::Error::from).and_then(|r| r) {
        let _ = tokio::fs::remove_file(&out).await;
        return Err(e.into());
    }
    let file = tokio::fs::File::open(&out).await?;
    let len = file.metadata().await?.len();
    // Le fichier ouvert reste lisible après sa suppression.
    let _ = tokio::fs::remove_file(&out).await;
    let filename = format!("{id}-{name}.zip").replace('"', "");
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "application/zip")
        .header(header::CONTENT_LENGTH, len)
        .header(header::CONTENT_DISPOSITION, format!("attachment; filename=\"{filename}\""))
        .body(Body::from_stream(ReaderStream::new(file)))
        .map_err(anyhow::Error::from)?)
}
