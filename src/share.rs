//! Pack client à partager avec les joueurs.
//!
//! Chaque instance moddée peut avoir un dossier `client/` (contenu d'un
//! `.minecraft` : `mods/`, `config/`, `resourcepacks/`…), rempli par l'import
//! d'un modpack ou préparé depuis les mods du serveur. Un jeton secret ouvre,
//! sur un port à part (`OMG_SHARE_LISTEN`), une page avec deux téléchargements :
//!
//! - `prism.zip` : une instance Prism Launcher complète (loader, version, mods,
//!   configs, serveur déjà dans la liste multijoueur), importable par URL ;
//! - `mods.zip` : le contenu brut, pour l'installer à la main.
//!
//! Ce port ne sert que ça : le panel lui-même reste sur le sien, sans comptes,
//! réservé aux admins. Les zips sont mis en cache dans `.share-cache/` et
//! reconstruits quand le pack change.

use std::{
    io::{Read, Write},
    path::{Path as FsPath, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use tokio_util::io::ReaderStream;

use crate::{
    app::App,
    error::{ApiResult, AppError},
    instances::{Instance, Kind},
};

type AppState = State<Arc<App>>;

const CACHE: &str = ".share-cache";

fn new_token() -> Result<String> {
    let mut b = [0u8; 24];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(hex::encode(b))
}

fn client_dir(app: &App, id: &str) -> PathBuf {
    app.instance_dir(id).join("client")
}

/// Fichiers du pack client : (chemin relatif, chemin absolu, taille, mtime).
fn walk(root: &FsPath) -> Vec<(String, PathBuf, u64, u64)> {
    let mut out = vec![];
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(m) = e.metadata() else { continue };
            if m.is_dir() {
                stack.push(p);
            } else if m.is_file() {
                let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
                let mtime = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                out.push((rel, p, m.len(), mtime));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Composants Prism (`mmc-pack.json`) : Minecraft et le loader de l'instance.
fn mmc_pack(inst: &Instance) -> Value {
    let mut c = vec![json!({ "uid": "net.minecraft", "version": inst.mc_version, "important": true })];
    let lv = inst.loader_version.clone().unwrap_or_default();
    // Les dépendances (intermediary, LWJGL…) sont résolues par Prism d'après
    // ses métadonnées : intermediary n'existe plus pour les versions non
    // obfusquées (26.1+), le figer ici casserait l'import.
    match inst.kind {
        Kind::Fabric => c.push(json!({ "uid": "net.fabricmc.fabric-loader", "version": lv })),
        Kind::Quilt => c.push(json!({ "uid": "org.quiltmc.quilt-loader", "version": lv })),
        Kind::Forge => c.push(json!({ "uid": "net.minecraftforge", "version": lv })),
        Kind::Neoforge => c.push(json!({ "uid": "net.neoforged", "version": lv })),
        _ => {}
    }
    json!({ "formatVersion": 1, "components": c })
}

fn loader_label(inst: &Instance) -> String {
    let name = match inst.kind {
        Kind::Fabric => "Fabric",
        Kind::Quilt => "Quilt",
        Kind::Forge => "Forge",
        Kind::Neoforge => "NeoForge",
        Kind::Paper => "Paper",
        Kind::Vanilla => "Vanilla",
        Kind::Custom => "personnalisé",
    };
    match &inst.loader_version {
        Some(v) if inst.kind.modded() => format!("{name} {v}"),
        _ => name.to_string(),
    }
}

/// `servers.dat` (NBT non compressé) avec une seule entrée : le serveur
/// apparaît tout seul dans la liste multijoueur.
pub fn servers_dat(name: &str, ip: &str) -> Vec<u8> {
    fn s(out: &mut Vec<u8>, v: &str) {
        out.extend((v.len() as u16).to_be_bytes());
        out.extend(v.as_bytes());
    }
    let mut o = vec![0x0A, 0, 0]; // compound racine, nom vide
    o.push(0x09); // liste
    s(&mut o, "servers");
    o.push(0x0A);
    o.extend(1i32.to_be_bytes());
    o.push(0x08);
    s(&mut o, "name");
    s(&mut o, name);
    o.push(0x08);
    s(&mut o, "ip");
    s(&mut o, ip);
    o.push(0x00); // fin de l'entrée
    o.push(0x00); // fin de la racine
    o
}

fn stored(name: &str) -> bool {
    [".jar", ".zip", ".png", ".jpg", ".ogg", ".gz", ".mrpack"].iter().any(|e| name.ends_with(e))
}

/// Construit le zip demandé dans `out`.
fn build_zip(app: &App, inst: &Instance, prism: bool, files: &[(String, PathBuf, u64, u64)], out: &FsPath) -> Result<()> {
    let mut w = zip::ZipWriter::new(std::io::BufWriter::new(std::fs::File::create(out)?));
    let deflate = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);
    let store = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .large_file(true);
    let (root, game) = if prism {
        (format!("{}/", inst.id), format!("{}/.minecraft/", inst.id))
    } else {
        (String::new(), String::new())
    };
    if prism {
        let mem = inst.memory_max_mb.clamp(4096, 8192);
        w.start_file(format!("{root}instance.cfg"), deflate)?;
        write!(
            w,
            "InstanceType=OneSix\nname={}\niconKey=default\nOverrideMemory=true\nMinMemAlloc=1024\nMaxMemAlloc={mem}\n",
            inst.name.replace('\n', " ")
        )?;
        w.start_file(format!("{root}mmc-pack.json"), deflate)?;
        w.write_all(serde_json::to_string_pretty(&mmc_pack(inst))?.as_bytes())?;
    } else {
        w.start_file("LISEZMOI.txt", deflate)?;
        write!(
            w,
            "{name}\r\n\r\nMinecraft {mc}, {loader}.\r\n\r\n\
             1. Installez {loader} pour Minecraft {mc} dans votre launcher.\r\n\
             2. Copiez le contenu de ce zip dans le dossier du jeu (.minecraft),\r\n   en remplaçant les fichiers existants.\r\n\
             3. Lancez le jeu : le serveur « {name} » est déjà dans la liste multijoueur ({addr}).\r\n",
            name = inst.name,
            mc = inst.mc_version,
            loader = loader_label(inst),
            addr = app.cfg.public_address,
        )?;
    }
    let mut has_servers = false;
    let mut buf = vec![0u8; 1 << 16];
    for (rel, path, _, _) in files {
        if rel == "servers.dat" {
            has_servers = true;
        }
        w.start_file(format!("{game}{rel}"), if stored(rel) { store } else { deflate })?;
        let mut f = std::fs::File::open(path).with_context(|| format!("lecture de {rel}"))?;
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            w.write_all(&buf[..n])?;
        }
    }
    if !has_servers && !app.cfg.public_address.is_empty() {
        w.start_file(format!("{game}servers.dat"), store)?;
        w.write_all(&servers_dat(&inst.name, &app.cfg.public_address))?;
    }
    w.finish()?.flush()?;
    Ok(())
}

/// Zip à jour du pack client (construit au besoin, puis mis en cache).
async fn cached_zip(app: &Arc<App>, inst: &Instance, prism: bool) -> ApiResult<PathBuf> {
    let root = client_dir(app, &inst.id);
    if !root.is_dir() {
        return Err(AppError::not_found("pas de pack client pour ce serveur"));
    }
    let _lock = app.share_lock.lock().await;
    let r = root.clone();
    let files = tokio::task::spawn_blocking(move || walk(&r)).await?;
    // Empreinte : contenu du pack (noms, tailles, dates) et tout ce qui finit
    // dans les fichiers générés.
    let mut h = Sha1::new();
    for (rel, _, size, mtime) in &files {
        h.update(format!("{rel}\0{size}\0{mtime}\n"));
    }
    h.update(serde_json::to_vec(&mmc_pack(inst))?);
    h.update(format!("{}\0{}\0{}\0{}", inst.name, inst.memory_max_mb, app.cfg.public_address, 2));
    let kind = if prism { "prism" } else { "mods" };
    let fp = hex::encode(h.finalize());
    let cache = app.instance_dir(&inst.id).join(CACHE);
    tokio::fs::create_dir_all(&cache).await?;
    let out = cache.join(format!("{kind}-{}.zip", &fp[..16]));
    if out.is_file() {
        return Ok(out);
    }
    app.server.log(format!("[omg] [{}] préparation du pack client ({kind}.zip, {} fichiers)", inst.id, files.len()));
    let tmp = cache.join(format!(".{kind}.partial"));
    let (a, i, t) = (app.clone(), inst.clone(), tmp.clone());
    let res = tokio::task::spawn_blocking(move || build_zip(&a, &i, prism, &files, &t)).await?;
    if let Err(e) = res {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e.into());
    }
    // Les versions précédentes du même zip ne servent plus.
    if let Ok(rd) = std::fs::read_dir(&cache) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with(&format!("{kind}-")) && n.ends_with(".zip") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    tokio::fs::rename(&tmp, &out).await?;
    Ok(out)
}

fn find_by_token(app: &App, token: &str) -> Option<Instance> {
    if token.len() != 48 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    app.list_instances()
        .ok()?
        .into_iter()
        .find(|i| i.share_token.as_deref() == Some(token) && i.kind.modded())
}

// --- API d'administration ---------------------------------------------------------

fn share_url(app: &App, token: &str) -> String {
    format!("{}/p/{token}", app.cfg.share_url.trim_end_matches('/'))
}

pub async fn status(State(app): AppState, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    app.load_instance(&id)?;
    app.reconcile_missing(&id);
    let inst = app.load_instance(&id)?;
    let root = client_dir(&app, &id);
    let exists = root.is_dir();
    let files = if exists { tokio::task::spawn_blocking(move || walk(&root)).await? } else { vec![] };
    let mods = files.iter().filter(|f| f.0.starts_with("mods/")).count();
    Ok(Json(json!({
        "client": exists,
        "files": files.len(),
        "mods": mods,
        "size": files.iter().map(|f| f.2).sum::<u64>(),
        "token": inst.share_token,
        "url": inst.share_token.as_deref().map(|t| share_url(&app, t)),
        "loader": loader_label(&inst),
        "missing": inst.modpack.as_ref().map(|m| m.missing.clone()).unwrap_or_default(),
    })))
}

/// Crée (ou remplace, ce qui invalide l'ancien lien) le jeton de partage.
pub async fn enable(State(app): AppState, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let inst = app.load_instance(&id)?;
    if !inst.kind.modded() {
        return Err(AppError::bad("seules les instances moddées ont un pack client"));
    }
    let token = new_token()?;
    app.update_instance(&id, |i| i.share_token = Some(token.clone()))?;
    app.server.log(format!("[omg] [{id}] nouveau lien de téléchargement du pack client"));
    Ok(Json(json!({ "token": token, "url": share_url(&app, &token) })))
}

pub async fn disable(State(app): AppState, Path(id): Path<String>) -> ApiResult<StatusCode> {
    app.update_instance(&id, |i| i.share_token = None)?;
    app.server.log(format!("[omg] [{id}] lien de téléchargement désactivé"));
    Ok(StatusCode::NO_CONTENT)
}

/// Prépare (ou complète) `client/` depuis le serveur : liens physiques vers
/// les mods actifs, copie de `config/` pour ce qui n'y est pas déjà.
pub async fn from_server(State(app): AppState, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let inst = app.load_instance(&id)?;
    if !inst.kind.modded() {
        return Err(AppError::bad("seules les instances moddées ont un pack client"));
    }
    let server = app.instance_dir(&id);
    let client = client_dir(&app, &id);
    let n = tokio::task::spawn_blocking(move || -> Result<usize> {
        let mut n = 0;
        std::fs::create_dir_all(client.join("mods"))?;
        if let Ok(rd) = std::fs::read_dir(server.join("mods")) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if !name.ends_with(".jar") || !e.path().is_file() {
                    continue;
                }
                let dest = client.join("mods").join(&name);
                if dest.exists() {
                    continue;
                }
                if std::fs::hard_link(e.path(), &dest).is_err() {
                    std::fs::copy(e.path(), &dest)?;
                }
                n += 1;
            }
        }
        let cfg = server.join("config");
        for (rel, src, _, _) in walk(&cfg) {
            let dest = client.join("config").join(&rel);
            if dest.exists() {
                continue;
            }
            if let Some(p) = dest.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::copy(&src, &dest)?;
            n += 1;
        }
        Ok(n)
    })
    .await??;
    app.server.log(format!("[omg] [{id}] pack client : {n} fichiers repris du serveur"));
    Ok(Json(json!({ "added": n })))
}

// --- surface joueurs (port à part) ------------------------------------------------

pub fn public_router(app: Arc<App>) -> Router {
    Router::new()
        .route("/p/{token}", get(page))
        .route("/p/{token}/prism.zip", get(prism_zip))
        .route("/p/{token}/mods.zip", get(mods_zip))
        .fallback(|| async { (StatusCode::NOT_FOUND, "introuvable") })
        .with_state(app)
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

async fn page(State(app): AppState, Path(token): Path<String>) -> Response {
    let Some(inst) = find_by_token(&app, &token) else {
        return (StatusCode::NOT_FOUND, "lien invalide ou désactivé").into_response();
    };
    let pack = inst
        .modpack
        .as_ref()
        .map(|m| format!("{} {}", m.title, m.version_name))
        .unwrap_or_default();
    let html = PAGE
        .replace("{NAME}", &esc(&inst.name))
        .replace("{PACK}", &esc(pack.trim()))
        .replace("{MC}", &esc(&inst.mc_version))
        .replace("{LOADER}", &esc(&loader_label(&inst)))
        .replace("{ADDR}", &esc(&app.cfg.public_address))
        .replace("{TOKEN}", &token);
    Html(html).into_response()
}

async fn serve_zip(app: Arc<App>, token: String, prism: bool) -> Response {
    let Some(inst) = find_by_token(&app, &token) else {
        return (StatusCode::NOT_FOUND, "lien invalide ou désactivé").into_response();
    };
    let path = match cached_zip(&app, &inst, prism).await {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let Ok(f) = tokio::fs::File::open(&path).await else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "zip illisible").into_response();
    };
    let len = f.metadata().await.map(|m| m.len()).unwrap_or(0);
    let name = format!("{}-{}.zip", inst.id, if prism { "prism" } else { "mods" });
    Response::builder()
        .header(header::CONTENT_TYPE, "application/zip")
        .header(header::CONTENT_LENGTH, len)
        .header(header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\""))
        .body(Body::from_stream(ReaderStream::new(f)))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn prism_zip(State(app): AppState, Path(token): Path<String>) -> Response {
    serve_zip(app, token, true).await
}

async fn mods_zip(State(app): AppState, Path(token): Path<String>) -> Response {
    serve_zip(app, token, false).await
}

const PAGE: &str = r#"<!doctype html>
<html lang="fr"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>{NAME} — modpack</title>
<style>
:root{color-scheme:dark;--bg:#16181c;--panel:#1f2228;--border:#2e323a;--text:#e6e8eb;--muted:#9aa1ab;--accent:#5fbf3f}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--text);font:16px/1.55 system-ui,sans-serif}
main{max-width:720px;margin:0 auto;padding:32px 16px 48px}h1{margin:0 0 4px;font-size:1.6rem}
.muted{color:var(--muted)}.card{background:var(--panel);border:1px solid var(--border);border-radius:10px;padding:18px 20px;margin-top:18px}
h2{margin:0 0 8px;font-size:1.1rem}ol{margin:8px 0 0;padding-left:1.3rem}li{margin:6px 0}
code,.copy input{font-family:ui-monospace,Menlo,monospace;font-size:.88rem}
.copy{display:flex;gap:8px;margin-top:8px}.copy input{flex:1;min-width:0;background:var(--bg);color:var(--text);border:1px solid var(--border);border-radius:6px;padding:8px}
button,.btn{background:#3d8f25;color:#fff;border:0;border-radius:6px;padding:9px 14px;font:inherit;cursor:pointer;text-decoration:none;display:inline-block}
.btn.sec{background:var(--border)}a{color:var(--accent)}
</style></head><body><main>
<h1>{NAME}</h1>
<p class="muted">{PACK} · Minecraft {MC} · {LOADER} · serveur <code>{ADDR}</code></p>

<div class="card">
<h2>Recommandé : Prism Launcher</h2>
<ol>
<li>Installez <a href="https://prismlauncher.org/download/" target="_blank" rel="noopener">Prism Launcher</a> (gratuit) et ajoutez-y votre compte Microsoft.</li>
<li>Cliquez sur <strong>Ajouter une instance</strong> → <strong>Importer</strong>, et collez ce lien :
<div class="copy"><input id="prism" readonly><button onclick="cp()">Copier</button></div></li>
<li>Lancez l'instance : tout est déjà installé, et le serveur est dans la liste <strong>Multijoueur</strong>.</li>
</ol>
<p class="muted">Ou téléchargez le fichier et glissez-le dans Prism : <a href="{TOKEN}/prism.zip" id="pz">prism.zip</a></p>
</div>

<div class="card">
<h2>À la main</h2>
<p>Installez <strong>{LOADER}</strong> pour Minecraft {MC}, puis copiez le contenu de ce zip dans votre dossier <code>.minecraft</code>.</p>
<a class="btn sec" href="{TOKEN}/mods.zip">Télécharger mods.zip</a>
</div>
<p class="muted">La première préparation du pack peut prendre une minute.</p>
</main>
<script>
const base=location.href.replace(/\/+$/,'');
document.getElementById('prism').value=base+'/prism.zip';
document.getElementById('pz').href=base+'/prism.zip';
document.querySelector('a.btn.sec').href=base+'/mods.zip';
function cp(){const i=document.getElementById('prism');i.select();navigator.clipboard&&navigator.clipboard.writeText(i.value)}
</script>
</body></html>"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nbt_servers() {
        let d = servers_dat("A", "mc.x");
        assert_eq!(&d[..3], &[0x0A, 0, 0]);
        assert_eq!(d.last(), Some(&0));
        let s = String::from_utf8_lossy(&d);
        assert!(s.contains("servers") && s.contains("mc.x"));
    }
}
