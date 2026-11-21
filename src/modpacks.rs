//! Modpacks Modrinth (`.mrpack`) et CurseForge (`.zip` + `manifest.json`).
//!
//! Un import crée une instance dont le serveur reçoit les fichiers côté
//! serveur, et dont le dossier `client/` reçoit les fichiers côté client : c'est
//! le pack que les joueurs téléchargent (voir `share`). Un fichier utile des deux
//! côtés n'est stocké qu'une fois (lien physique).
//!
//! CurseForge laisse chaque auteur interdire la distribution par des outils
//! tiers : ces fichiers n'ont pas d'URL de téléchargement. Ils sont listés dans
//! `ModpackInfo::missing`, avec le lien de leur page, pour être ajoutés à la main.

use std::{
    collections::HashMap,
    io::Read,
    path::{Path as FsPath, PathBuf},
    sync::Arc,
};

use anyhow::{anyhow, bail, Context, Result};
use axum::{
    extract::{Multipart, Path, Query, State},
    Json,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

use crate::{
    app::App,
    error::{ApiResult, AppError},
    install::{self, download},
    instances::{safe_name, Instance, Kind},
    routes::check_memory,
};

type AppState = State<Arc<App>>;

const MODRINTH: &str = "https://api.modrinth.com/v2";
const CURSEFORGE: &str = "https://api.curseforge.com/v1";
const CF_GAME: u32 = 432;
const CF_MODPACKS: u32 = 4471;

/// Ce qui ne sert à rien sur un serveur dans les `overrides` d'un pack.
const CLIENT_ONLY_TOP: [&str; 8] = [
    "resourcepacks",
    "shaderpacks",
    "screenshots",
    "options.txt",
    "optionsof.txt",
    "optionsshaders.txt",
    "servers.dat",
    "saves",
];

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MissingFile {
    pub file_name: String,
    /// Page où le télécharger à la main.
    pub url: Option<String>,
    /// Dossier de destination (`mods`, `resourcepacks`…).
    pub dir: String,
    pub server: bool,
    pub client: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ModpackInfo {
    /// `modrinth`, `curseforge` ou `fichier`.
    pub source: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub version_id: Option<String>,
    pub title: String,
    pub version_name: String,
    #[serde(default)]
    pub icon_url: Option<String>,
    #[serde(default)]
    pub missing: Vec<MissingFile>,
}

/// Archive de modpack téléchargée, prête à installer.
pub struct PackSource {
    pub path: PathBuf,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Format {
    Modrinth,
    CurseForge,
}

/// Ce qu'il faut savoir d'un pack avant de créer l'instance.
#[derive(Debug)]
struct Header {
    format: Format,
    name: String,
    version: String,
    mc: String,
    kind: Kind,
    loader_version: Option<String>,
}

// --- lecture des archives ------------------------------------------------------

#[derive(Deserialize)]
struct MrIndex {
    #[serde(default)]
    name: String,
    #[serde(rename = "versionId", default)]
    version_id: String,
    files: Vec<MrFile>,
    dependencies: HashMap<String, String>,
}

#[derive(Deserialize)]
struct MrFile {
    path: String,
    hashes: HashMap<String, String>,
    #[serde(default)]
    env: Option<MrEnv>,
    downloads: Vec<String>,
}

#[derive(Deserialize)]
struct MrEnv {
    client: String,
    server: String,
}

#[derive(Deserialize)]
struct CfManifest {
    minecraft: CfMinecraft,
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    files: Vec<CfManifestFile>,
    #[serde(default = "default_overrides")]
    overrides: String,
}

fn default_overrides() -> String {
    "overrides".into()
}

#[derive(Deserialize)]
struct CfMinecraft {
    version: String,
    #[serde(rename = "modLoaders", default)]
    mod_loaders: Vec<CfLoader>,
}

#[derive(Deserialize)]
struct CfLoader {
    id: String,
    #[serde(default)]
    primary: bool,
}

#[derive(Deserialize)]
struct CfManifestFile {
    #[serde(rename = "projectID")]
    project_id: u64,
    #[serde(rename = "fileID")]
    file_id: u64,
    #[serde(default = "yes")]
    required: bool,
}

fn yes() -> bool {
    true
}

fn read_entry(path: &FsPath, name: &str) -> Result<Option<Vec<u8>>> {
    let mut z = zip::ZipArchive::new(std::fs::File::open(path)?)?;
    let res = match z.by_name(name) {
        Ok(mut f) => {
            let mut buf = vec![];
            f.read_to_end(&mut buf)?;
            Some(buf)
        }
        Err(zip::result::ZipError::FileNotFound) => None,
        Err(e) => return Err(e.into()),
    };
    Ok(res)
}

/// `fabric-0.16.10` → (Fabric, 0.16.10) ; `forge-47.2.0` → (Forge, 47.2.0).
fn parse_cf_loader(id: &str) -> Result<(Kind, String)> {
    let (name, v) = id.split_once('-').ok_or_else(|| anyhow!("loader inconnu : {id}"))?;
    let kind = match name {
        "forge" => Kind::Forge,
        "neoforge" => Kind::Neoforge,
        "fabric" => Kind::Fabric,
        "quilt" => Kind::Quilt,
        _ => bail!("loader non pris en charge : {id}"),
    };
    // NeoForge 1.20.1 publiait sous l'ancien nom (`neoforge-1.20.1-47.1.x`).
    if kind == Kind::Neoforge && v.starts_with("1.") {
        bail!("NeoForge pour 1.20.1 n'est pas pris en charge");
    }
    Ok((kind, v.to_string()))
}

fn read_header(path: &FsPath) -> Result<Header> {
    if let Some(raw) = read_entry(path, "modrinth.index.json")? {
        let idx: MrIndex = serde_json::from_slice(&raw).context("modrinth.index.json illisible")?;
        let mc = idx
            .dependencies
            .get("minecraft")
            .cloned()
            .ok_or_else(|| anyhow!("le pack ne précise pas sa version de Minecraft"))?;
        let (kind, loader_version) = [
            ("neoforge", Kind::Neoforge),
            ("forge", Kind::Forge),
            ("fabric-loader", Kind::Fabric),
            ("quilt-loader", Kind::Quilt),
        ]
        .into_iter()
        .find_map(|(k, kind)| idx.dependencies.get(k).map(|v| (kind, Some(v.clone()))))
        .unwrap_or((Kind::Vanilla, None));
        return Ok(Header {
            format: Format::Modrinth,
            name: idx.name,
            version: idx.version_id,
            mc,
            kind,
            loader_version,
        });
    }
    if let Some(raw) = read_entry(path, "manifest.json")? {
        let m: CfManifest = serde_json::from_slice(&raw).context("manifest.json illisible")?;
        let loader = m
            .minecraft
            .mod_loaders
            .iter()
            .find(|l| l.primary)
            .or(m.minecraft.mod_loaders.first());
        let (kind, loader_version) = match loader {
            Some(l) => {
                let (k, v) = parse_cf_loader(&l.id)?;
                (k, Some(v))
            }
            None => (Kind::Vanilla, None),
        };
        return Ok(Header {
            format: Format::CurseForge,
            name: m.name,
            version: m.version,
            mc: m.minecraft.version,
            kind,
            loader_version,
        });
    }
    bail!("ni modrinth.index.json ni manifest.json : ce n'est pas un modpack")
}

/// Extrait les entrées `prefix/…` de l'archive vers chacune des destinations
/// (`dest`, `exclure à la racine`). Renvoie le nombre de fichiers écrits.
fn extract_prefix(zip_path: &FsPath, prefix: &str, dests: &[(PathBuf, &[&str])]) -> Result<usize> {
    let mut z = zip::ZipArchive::new(std::fs::File::open(zip_path)?)?;
    let prefix = format!("{}/", prefix.trim_end_matches('/'));
    let mut n = 0;
    for i in 0..z.len() {
        let mut f = z.by_index(i)?;
        let Some(name) = f.enclosed_name() else { continue };
        let Ok(rel) = name.strip_prefix(&prefix) else { continue };
        if rel.as_os_str().is_empty() || f.is_dir() {
            continue;
        }
        let top = rel.components().next().map(|c| c.as_os_str().to_string_lossy().into_owned()).unwrap_or_default();
        let mut buf = vec![];
        f.read_to_end(&mut buf)?;
        for (dest, exclude) in dests {
            if exclude.contains(&top.as_str()) {
                continue;
            }
            let out = dest.join(rel);
            if let Some(p) = out.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::write(&out, &buf)?;
            n += 1;
        }
    }
    Ok(n)
}

// --- installation --------------------------------------------------------------

/// Un fichier à poser côté serveur et/ou côté client.
struct Planned {
    rel: String,
    url: String,
    sha1: Option<String>,
    server: bool,
    client: bool,
}

/// Télécharge un fichier une fois et le pose aux deux emplacements voulus.
async fn fetch_planned(http: reqwest::Client, server_root: PathBuf, client_root: PathBuf, f: Planned) -> Result<()> {
    let mut targets = vec![];
    if f.server {
        targets.push(server_root.join(&f.rel));
    }
    if f.client {
        targets.push(client_root.join(&f.rel));
    }
    let Some(first) = targets.first() else { return Ok(()) };
    for t in &targets {
        if let Some(p) = t.parent() {
            tokio::fs::create_dir_all(p).await?;
        }
    }
    let mut last = None;
    for attempt in 0..3 {
        match download(&http, &f.url, first, f.sha1.as_deref()).await {
            Ok(()) => {
                last = None;
                break;
            }
            Err(e) => {
                last = Some(e);
                tokio::time::sleep(std::time::Duration::from_secs(1 + attempt)).await;
            }
        }
    }
    if let Some(e) = last {
        return Err(e.context(format!("téléchargement de {}", f.rel)));
    }
    for t in targets.iter().skip(1) {
        let _ = tokio::fs::remove_file(t).await;
        if std::fs::hard_link(first, t).is_err() {
            tokio::fs::copy(first, t).await?;
        }
    }
    Ok(())
}

async fn fetch_all(app: &App, id: &str, files: Vec<Planned>) -> Result<()> {
    let server_root = app.instance_dir(id);
    let client_root = server_root.join("client");
    let total = files.len();
    app.server.log(format!("[omg] [{id}] téléchargement de {total} fichiers du modpack"));
    let mut done = 0usize;
    let mut errors = vec![];
    let http = app.http.clone();
    let mut stream = futures_util::stream::iter(files)
        .map(move |f| {
            let (h, s, c) = (http.clone(), server_root.clone(), client_root.clone());
            tokio::spawn(fetch_planned(h, s, c, f))
        })
        .buffer_unordered(8);
    while let Some(r) = stream.next().await {
        done += 1;
        match r {
            Ok(Ok(())) => {}
            Ok(Err(e)) => errors.push(format!("{e:#}")),
            Err(e) => errors.push(e.to_string()),
        }
        if done % 25 == 0 || done == total {
            app.server.log(format!("[omg] [{id}] {done}/{total} fichiers"));
        }
    }
    if !errors.is_empty() {
        bail!("{} fichier(s) en échec : {}", errors.len(), errors.join(" ; "));
    }
    Ok(())
}

/// Chemin d'un fichier de pack : relatif, sans `.`/`..`. Les dossiers cachés
/// sont permis (certains packs en livrent, ex. `mods/.connector/`).
fn safe_pack_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 400
        && !p.starts_with('/')
        && !p.chars().any(|c| c.is_control() || c == '\\')
        && p.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}

fn mr_side(v: &str) -> bool {
    v != "unsupported"
}

async fn install_modrinth(app: &App, id: &str, path: &FsPath) -> Result<Vec<MissingFile>> {
    let raw = read_entry(path, "modrinth.index.json")?.ok_or_else(|| anyhow!("index manquant"))?;
    let idx: MrIndex = serde_json::from_slice(&raw)?;
    let mut planned = vec![];
    for f in idx.files {
        if !safe_pack_path(&f.path) {
            bail!("chemin refusé dans le pack : {}", f.path);
        }
        let (server, client) = match &f.env {
            Some(e) => (mr_side(&e.server), mr_side(&e.client)),
            None => (true, true),
        };
        let top = f.path.split('/').next().unwrap_or("");
        let server = server && !CLIENT_ONLY_TOP.contains(&top);
        let url = f.downloads.first().cloned().ok_or_else(|| anyhow!("{} : aucune URL", f.path))?;
        planned.push(Planned { rel: f.path, url, sha1: f.hashes.get("sha1").cloned(), server, client });
    }
    fetch_all(app, id, planned).await?;
    extract_overrides(app, id, path, &["overrides"], &["server-overrides"], &["client-overrides"]).await?;
    Ok(vec![])
}

async fn extract_overrides(
    app: &App,
    id: &str,
    path: &FsPath,
    both: &[&str],
    server_only: &[&str],
    client_only: &[&str],
) -> Result<()> {
    let server = app.instance_dir(id);
    let client = server.join("client");
    let p = path.to_path_buf();
    let both: Vec<String> = both.iter().map(|s| s.to_string()).collect();
    let so: Vec<String> = server_only.iter().map(|s| s.to_string()).collect();
    let co: Vec<String> = client_only.iter().map(|s| s.to_string()).collect();
    let n = tokio::task::spawn_blocking(move || -> Result<usize> {
        let mut n = 0;
        for prefix in &both {
            n += extract_prefix(&p, prefix, &[(server.clone(), &CLIENT_ONLY_TOP), (client.clone(), &[])])?;
        }
        for prefix in &so {
            n += extract_prefix(&p, prefix, &[(server.clone(), &[])])?;
        }
        for prefix in &co {
            n += extract_prefix(&p, prefix, &[(client.clone(), &[])])?;
        }
        Ok(n)
    })
    .await??;
    app.server.log(format!("[omg] [{id}] {n} fichiers de configuration du pack posés"));
    Ok(())
}

#[derive(Deserialize)]
struct CfData<T> {
    data: T,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct CfFile {
    id: u64,
    mod_id: u64,
    #[serde(default)]
    display_name: String,
    file_name: String,
    #[serde(default)]
    download_url: Option<String>,
    #[serde(default)]
    hashes: Vec<CfHash>,
    #[serde(default)]
    game_versions: Vec<String>,
    #[serde(default)]
    file_date: String,
    #[serde(default)]
    is_server_pack: bool,
}

#[derive(Deserialize, Clone)]
struct CfHash {
    value: String,
    algo: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CfMod {
    id: u64,
    #[serde(default)]
    class_id: Option<u32>,
    #[serde(default)]
    links: Option<CfLinks>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CfLinks {
    #[serde(default)]
    website_url: Option<String>,
}

fn cf_key(app: &App) -> Result<&str> {
    app.cfg
        .curseforge_api_key
        .as_deref()
        .ok_or_else(|| anyhow!("pas de clé API CurseForge configurée (OMG_CURSEFORGE_API_KEY)"))
}

async fn cf_get<T: serde::de::DeserializeOwned>(app: &App, path: &str) -> Result<T> {
    let r = app
        .http
        .get(format!("{CURSEFORGE}{path}"))
        .header("x-api-key", cf_key(app)?)
        .header("accept", "application/json")
        .send()
        .await?;
    if r.status() == reqwest::StatusCode::FORBIDDEN {
        bail!("CurseForge refuse la clé API (403)");
    }
    Ok(r.error_for_status()?.json::<CfData<T>>().await?.data)
}

async fn cf_post<T: serde::de::DeserializeOwned>(app: &App, path: &str, body: Value) -> Result<T> {
    let r = app
        .http
        .post(format!("{CURSEFORGE}{path}"))
        .header("x-api-key", cf_key(app)?)
        .header("accept", "application/json")
        .json(&body)
        .send()
        .await?;
    Ok(r.error_for_status()?.json::<CfData<T>>().await?.data)
}

/// Le fichier est-il marqué pour un seul côté ? CurseForge l'indique parfois
/// dans `gameVersions` (« Client » / « Server »).
fn cf_sides(f: &CfFile) -> (bool, bool) {
    let c = f.game_versions.iter().any(|v| v == "Client");
    let s = f.game_versions.iter().any(|v| v == "Server");
    match (c, s) {
        (true, false) => (false, true),
        (false, true) => (true, false),
        _ => (true, true),
    }
}

async fn install_curseforge(app: &App, id: &str, path: &FsPath) -> Result<Vec<MissingFile>> {
    let raw = read_entry(path, "manifest.json")?.ok_or_else(|| anyhow!("manifest manquant"))?;
    let m: CfManifest = serde_json::from_slice(&raw)?;
    let wanted: Vec<&CfManifestFile> = m.files.iter().filter(|f| f.required).collect();
    let mut planned = vec![];
    let mut missing = vec![];
    if !wanted.is_empty() {
        let mut files: Vec<CfFile> = vec![];
        let mut mods: HashMap<u64, CfMod> = HashMap::new();
        for chunk in wanted.chunks(500) {
            let ids: Vec<u64> = chunk.iter().map(|f| f.file_id).collect();
            files.extend(cf_post::<Vec<CfFile>>(app, "/mods/files", json!({ "fileIds": ids })).await?);
            let mids: Vec<u64> = chunk.iter().map(|f| f.project_id).collect();
            for md in cf_post::<Vec<CfMod>>(app, "/mods", json!({ "modIds": mids })).await? {
                mods.insert(md.id, md);
            }
        }
        if files.len() < wanted.len() {
            app.server.log(format!(
                "[omg] [{id}] CurseForge n'a renvoyé que {} fichiers sur {}",
                files.len(),
                wanted.len()
            ));
        }
        for f in files {
            let md = mods.get(&f.mod_id);
            let dir = match md.and_then(|m| m.class_id) {
                Some(12) => "resourcepacks",
                Some(6552) => "shaderpacks",
                Some(6945) => {
                    app.server.log(format!("[omg] [{id}] datapack ignoré : {}", f.file_name));
                    continue;
                }
                _ => "mods",
            };
            safe_name(&f.file_name).map_err(|e| anyhow!(e.1))?;
            let (mut server, client) = cf_sides(&f);
            if dir != "mods" {
                server = false;
            }
            match &f.download_url {
                Some(url) => planned.push(Planned {
                    rel: format!("{dir}/{}", f.file_name),
                    url: url.clone(),
                    sha1: f.hashes.iter().find(|h| h.algo == 1).map(|h| h.value.clone()),
                    server,
                    client,
                }),
                None => missing.push(MissingFile {
                    file_name: f.file_name.clone(),
                    url: md
                        .and_then(|m| m.links.as_ref())
                        .and_then(|l| l.website_url.clone())
                        .map(|u| format!("{}/files/{}", u.trim_end_matches('/'), f.id)),
                    dir: dir.into(),
                    server,
                    client,
                }),
            }
        }
    }
    fetch_all(app, id, planned).await?;
    extract_overrides(app, id, path, &[m.overrides.as_str()], &[], &[]).await?;
    if !missing.is_empty() {
        app.server.log(format!(
            "[omg] [{id}] {} fichier(s) interdits de téléchargement automatique par leur auteur : à ajouter à la main (onglet Partage)",
            missing.len()
        ));
    }
    Ok(missing)
}

/// Pose le contenu du modpack sur une instance dont le loader est installé.
pub async fn install_pack(app: &App, id: &str, src: PackSource) -> Result<()> {
    let res = async {
        let header = read_header(&src.path)?;
        tokio::fs::create_dir_all(app.instance_dir(id).join("client")).await?;
        match header.format {
            Format::Modrinth => install_modrinth(app, id, &src.path).await,
            Format::CurseForge => install_curseforge(app, id, &src.path).await,
        }
    }
    .await;
    let _ = tokio::fs::remove_file(&src.path).await;
    let missing = res?;
    app.update_instance(id, |i| {
        if let Some(mp) = &mut i.modpack {
            mp.missing = missing;
        }
    })
    .map_err(|e| anyhow!(e.1))?;
    Ok(())
}

// --- mods purement client sur le serveur ------------------------------------------

/// Jars nommés par un rapport de crash Forge/NeoForge comme ayant chargé du
/// code client sur le serveur dédié (« invalid dist DEDICATED_SERVER »). Les
/// packs déclarent souvent mal ces mods ; il suffit de les désactiver.
pub fn client_only_culprits(report: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for block in report.split("-- Mod loading issue for:").skip(1) {
        let block = block.split("\n-- ").next().unwrap_or(block);
        if !block.contains("invalid dist DEDICATED_SERVER") {
            continue;
        }
        let jar = block
            .split(|c: char| c.is_whitespace() || c == '[' || c == '/' || c == '%')
            .find(|w| w.ends_with(".jar") && w.len() > 4);
        if let Some(j) = jar {
            if !out.iter().any(|o| o == j) {
                out.push(j.to_string());
            }
        }
    }
    out
}

impl App {
    /// Après un plantage au démarrage, signale dans la console les mods client
    /// en cause.
    pub fn spawn_crash_hints(self: &Arc<Self>) {
        let app = self.clone();
        tokio::spawn(async move {
            let mut st = app.server.watch_status();
            let mut prev = st.borrow().state;
            loop {
                tokio::select! {
                    _ = app.shutdown.cancelled() => return,
                    r = st.changed() => if r.is_err() { return },
                }
                let (state, inst) = {
                    let s = st.borrow_and_update();
                    (s.state, s.instance.clone())
                };
                let crashed_on_boot = prev == crate::server::RunState::Starting && state == crate::server::RunState::Stopped;
                prev = state;
                let Some(id) = inst.filter(|_| crashed_on_boot) else { continue };
                let dir = app.instance_dir(&id).join("crash-reports");
                let newest = std::fs::read_dir(&dir).ok().and_then(|rd| {
                    rd.flatten()
                        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
                        .max_by_key(|(t, _)| *t)
                        .filter(|(t, _)| t.elapsed().map(|d| d.as_secs() < 300).unwrap_or(false))
                });
                let Some((_, path)) = newest else { continue };
                let Ok(report) = std::fs::read_to_string(&path) else { continue };
                let jars = client_only_culprits(&report);
                if !jars.is_empty() {
                    app.server.log(format!(
                        "[omg] [{id}] démarrage impossible : mod(s) réservé(s) au client sur le serveur → désactivez dans l'onglet Mods : {}",
                        jars.join(", ")
                    ));
                }
            }
        });
    }
}

// --- API -----------------------------------------------------------------------

#[derive(Serialize)]
pub struct PackHit {
    id: String,
    title: String,
    description: String,
    icon_url: Option<String>,
    downloads: u64,
    game_versions: Vec<String>,
    loaders: Vec<String>,
}

#[derive(Serialize)]
pub struct PackVersion {
    id: String,
    name: String,
    game_versions: Vec<String>,
    loaders: Vec<String>,
    date: String,
}

#[derive(Deserialize)]
pub struct SearchQuery {
    source: String,
    #[serde(default)]
    q: String,
}

pub async fn config(State(app): AppState) -> Json<Value> {
    Json(json!({ "curseforge": app.cfg.curseforge_api_key.is_some() }))
}

pub async fn search(State(app): AppState, Query(q): Query<SearchQuery>) -> ApiResult<Json<Vec<PackHit>>> {
    let hits = match q.source.as_str() {
        "modrinth" => {
            #[derive(Deserialize)]
            struct R {
                hits: Vec<H>,
            }
            #[derive(Deserialize)]
            struct H {
                project_id: String,
                title: String,
                #[serde(default)]
                description: String,
                icon_url: Option<String>,
                downloads: u64,
                #[serde(default)]
                versions: Vec<String>,
                #[serde(default)]
                categories: Vec<String>,
            }
            let url = format!(
                "{MODRINTH}/search?limit=24&query={}&facets={}",
                urlencode(&q.q),
                urlencode(r#"[["project_type:modpack"]]"#)
            );
            let r: R = install_json(&app, &url).await?;
            r.hits
                .into_iter()
                .map(|h| {
                    let loaders = h
                        .categories
                        .into_iter()
                        .filter(|c| ["forge", "neoforge", "fabric", "quilt"].contains(&c.as_str()))
                        .collect();
                    let mut gv: Vec<String> = h.versions.into_iter().filter(|v| !v.contains('w')).collect();
                    gv.reverse();
                    gv.truncate(4);
                    PackHit {
                        id: h.project_id,
                        title: h.title,
                        description: h.description,
                        icon_url: h.icon_url,
                        downloads: h.downloads,
                        game_versions: gv,
                        loaders,
                    }
                })
                .collect()
        }
        "curseforge" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct M {
                id: u64,
                name: String,
                #[serde(default)]
                summary: String,
                logo: Option<Logo>,
                download_count: f64,
                #[serde(default)]
                latest_files_indexes: Vec<Idx>,
            }
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Logo {
                thumbnail_url: Option<String>,
            }
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Idx {
                game_version: String,
                #[serde(default)]
                mod_loader: Option<u32>,
            }
            let path = format!(
                "/mods/search?gameId={CF_GAME}&classId={CF_MODPACKS}&sortField=2&sortOrder=desc&pageSize=24&searchFilter={}",
                urlencode(&q.q)
            );
            let list: Vec<M> = cf_get(&app, &path).await?;
            list.into_iter()
                .map(|m| {
                    let mut gv: Vec<String> = vec![];
                    let mut loaders: Vec<String> = vec![];
                    for i in &m.latest_files_indexes {
                        if !gv.contains(&i.game_version) {
                            gv.push(i.game_version.clone());
                        }
                        let l = match i.mod_loader {
                            Some(1) => "forge",
                            Some(4) => "fabric",
                            Some(5) => "quilt",
                            Some(6) => "neoforge",
                            _ => continue,
                        };
                        if !loaders.iter().any(|x| x == l) {
                            loaders.push(l.into());
                        }
                    }
                    gv.truncate(4);
                    PackHit {
                        id: m.id.to_string(),
                        title: m.name,
                        description: m.summary,
                        icon_url: m.logo.and_then(|l| l.thumbnail_url),
                        downloads: m.download_count as u64,
                        game_versions: gv,
                        loaders,
                    }
                })
                .collect()
        }
        _ => return Err(AppError::bad("source inconnue")),
    };
    Ok(Json(hits))
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[derive(Deserialize)]
pub struct VersionsQuery {
    source: String,
    project: String,
}

fn check_project(id: &str) -> ApiResult<()> {
    if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(AppError::bad("identifiant de projet invalide"));
    }
    Ok(())
}

#[derive(Deserialize)]
struct MrVersion {
    id: String,
    name: String,
    #[serde(default)]
    version_number: String,
    game_versions: Vec<String>,
    loaders: Vec<String>,
    date_published: String,
    files: Vec<MrVersionFile>,
}

#[derive(Deserialize)]
struct MrVersionFile {
    url: String,
    #[serde(default)]
    primary: bool,
    hashes: HashMap<String, String>,
}

pub async fn versions(State(app): AppState, Query(q): Query<VersionsQuery>) -> ApiResult<Json<Vec<PackVersion>>> {
    check_project(&q.project)?;
    let list = match q.source.as_str() {
        "modrinth" => {
            let v: Vec<MrVersion> = install_json(&app, &format!("{MODRINTH}/project/{}/version", q.project)).await?;
            v.into_iter()
                .map(|v| PackVersion {
                    name: if v.name.is_empty() { v.version_number } else { v.name },
                    id: v.id,
                    game_versions: v.game_versions,
                    loaders: v.loaders,
                    date: v.date_published,
                })
                .collect()
        }
        "curseforge" => {
            let files: Vec<CfFile> = cf_get(&app, &format!("/mods/{}/files?pageSize=50", q.project)).await?;
            files
                .into_iter()
                .filter(|f| !f.is_server_pack)
                .map(|f| {
                    let loaders = f
                        .game_versions
                        .iter()
                        .filter(|g| ["Forge", "NeoForge", "Fabric", "Quilt"].contains(&g.as_str()))
                        .map(|g| g.to_lowercase())
                        .collect();
                    let gv = f
                        .game_versions
                        .iter()
                        .filter(|g| g.starts_with(|c: char| c.is_ascii_digit()))
                        .cloned()
                        .collect();
                    PackVersion {
                        id: f.id.to_string(),
                        name: if f.display_name.is_empty() { f.file_name } else { f.display_name },
                        game_versions: gv,
                        loaders,
                        date: f.file_date,
                    }
                })
                .collect()
        }
        _ => return Err(AppError::bad("source inconnue")),
    };
    Ok(Json(list))
}

async fn install_json<T: serde::de::DeserializeOwned>(app: &App, url: &str) -> Result<T> {
    Ok(app.http.get(url).send().await?.error_for_status()?.json().await?)
}

#[derive(Deserialize)]
pub struct ImportReq {
    source: String,
    project_id: String,
    version_id: String,
    #[serde(default)]
    name: String,
    memory_max_mb: Option<u32>,
    eula: bool,
    #[serde(default)]
    title: String,
    #[serde(default)]
    icon_url: Option<String>,
}

/// Crée l'instance à partir d'un pack déjà téléchargé dans `tmp/`, et lance
/// l'installation (loader, fichiers, configuration) en tâche de fond.
async fn create_from_pack(
    app: &Arc<App>,
    path: PathBuf,
    name: &str,
    max: u32,
    mut info: ModpackInfo,
) -> ApiResult<Instance> {
    let p = path.clone();
    let header = match tokio::task::spawn_blocking(move || read_header(&p)).await? {
        Ok(h) => h,
        Err(e) => {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(AppError::bad(format!("{e:#}")));
        }
    };
    if info.title.is_empty() {
        info.title = header.name.clone();
    }
    if info.version_name.is_empty() {
        info.version_name = header.version.clone();
    }
    let name = if name.trim().is_empty() {
        if header.name.is_empty() { "Modpack".to_string() } else { header.name.clone() }
    } else {
        name.trim().to_string()
    };
    let name: String = name.chars().take(60).collect();
    let inst = app.new_instance(&name, header.kind, &header.mc, header.loader_version.clone(), max)?;
    let inst = app.update_instance(&inst.id, |i| i.modpack = Some(info))?;
    app.server.log(format!(
        "[omg] [{}] import du modpack « {} » ({:?} {} pour {})",
        inst.id,
        header.name,
        header.kind,
        header.loader_version.as_deref().unwrap_or("-"),
        header.mc
    ));
    tokio::spawn(install::install_instance(app.clone(), inst.clone(), Some(PackSource { path })));
    Ok(inst)
}

pub async fn import(State(app): AppState, Json(req): Json<ImportReq>) -> ApiResult<Json<Instance>> {
    if !req.eula {
        return Err(AppError::bad("il faut accepter l'EULA de Minecraft"));
    }
    check_project(&req.project_id)?;
    check_project(&req.version_id)?;
    let max = req.memory_max_mb.unwrap_or(app.cfg.default_memory_mb);
    check_memory(max / 2, max)?;
    let (url, sha1) = match req.source.as_str() {
        "modrinth" => {
            let v: MrVersion = install_json(&app, &format!("{MODRINTH}/version/{}", req.version_id)).await?;
            let f = v
                .files
                .iter()
                .find(|f| f.primary)
                .or(v.files.first())
                .ok_or_else(|| AppError::bad("cette version n'a pas de fichier"))?;
            (f.url.clone(), f.hashes.get("sha1").cloned())
        }
        "curseforge" => {
            let f: CfFile = cf_get(&app, &format!("/mods/{}/files/{}", req.project_id, req.version_id)).await?;
            let url = f.download_url.clone().ok_or_else(|| {
                AppError::bad("l'auteur de ce modpack interdit son téléchargement automatique — téléchargez le .zip et importez-le comme fichier")
            })?;
            (url, f.hashes.iter().find(|h| h.algo == 1).map(|h| h.value.clone()))
        }
        _ => return Err(AppError::bad("source inconnue")),
    };
    let path = app.tmp_path("pack");
    download(&app.http, &url, &path, sha1.as_deref()).await?;
    let info = ModpackInfo {
        source: req.source.clone(),
        project_id: Some(req.project_id),
        version_id: Some(req.version_id),
        title: req.title,
        version_name: String::new(),
        icon_url: req.icon_url.filter(|u| u.starts_with("https://")),
        missing: vec![],
    };
    Ok(Json(create_from_pack(&app, path, &req.name, max, info).await?))
}

/// Import d'un `.mrpack` ou d'un `.zip` CurseForge envoyé depuis le navigateur.
pub async fn upload(State(app): AppState, mut mp: Multipart) -> ApiResult<Json<Instance>> {
    let mut name = String::new();
    let mut max = app.cfg.default_memory_mb;
    let mut eula = false;
    let mut path: Option<PathBuf> = None;
    let mut file_name = String::new();
    while let Some(mut field) = mp.next_field().await? {
        match field.name().unwrap_or("") {
            "name" => name = field.text().await?,
            "memory_max_mb" => max = field.text().await?.trim().parse().map_err(|_| AppError::bad("mémoire invalide"))?,
            "eula" => eula = field.text().await? == "true",
            "file" => {
                file_name = field.file_name().unwrap_or("modpack").to_string();
                let tmp = app.tmp_path("pack");
                let mut f = tokio::fs::File::create(&tmp).await?;
                while let Some(chunk) = field.chunk().await? {
                    f.write_all(&chunk).await?;
                }
                f.flush().await?;
                path = Some(tmp);
            }
            _ => {}
        }
    }
    let path = path.ok_or_else(|| AppError::bad("fichier manquant"))?;
    if !eula {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(AppError::bad("il faut accepter l'EULA de Minecraft"));
    }
    if let Err(e) = check_memory(max / 2, max) {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(e);
    }
    let info = ModpackInfo {
        source: "fichier".into(),
        project_id: None,
        version_id: None,
        title: String::new(),
        version_name: file_name,
        icon_url: None,
        missing: vec![],
    };
    Ok(Json(create_from_pack(&app, path, &name, max, info).await?))
}

/// Fichiers manquants (interdits au téléchargement automatique) ajoutés à la
/// main : chacun est reconnu par son nom et posé côté serveur et/ou client.
pub async fn upload_missing(
    State(app): AppState,
    Path(id): Path<String>,
    mut mp: Multipart,
) -> ApiResult<Json<Value>> {
    let inst = app.load_instance(&id)?;
    let Some(pack) = inst.modpack.clone() else {
        return Err(AppError::bad("cette instance ne vient pas d'un modpack"));
    };
    let dir = app.instance_dir(&id);
    let mut added = vec![];
    let mut unknown = vec![];
    while let Some(mut field) = mp.next_field().await? {
        let Some(name) = field.file_name().map(str::to_string) else { continue };
        safe_name(&name)?;
        let Some(m) = pack.missing.iter().find(|m| m.file_name == name) else {
            unknown.push(name);
            continue;
        };
        let tmp = app.tmp_path("upload");
        let mut f = tokio::fs::File::create(&tmp).await?;
        while let Some(chunk) = field.chunk().await? {
            f.write_all(&chunk).await?;
        }
        f.flush().await?;
        drop(f);
        let mut targets = vec![];
        if m.server {
            targets.push(dir.join(&m.dir).join(&name));
        }
        if m.client {
            targets.push(dir.join("client").join(&m.dir).join(&name));
        }
        for (i, t) in targets.iter().enumerate() {
            if let Some(p) = t.parent() {
                tokio::fs::create_dir_all(p).await?;
            }
            if i == 0 {
                tokio::fs::rename(&tmp, t).await?;
            } else if std::fs::hard_link(&targets[0], t).is_err() {
                tokio::fs::copy(&targets[0], t).await?;
            }
        }
        if targets.is_empty() {
            let _ = tokio::fs::remove_file(&tmp).await;
        }
        added.push(name);
    }
    app.update_instance(&id, |i| {
        if let Some(mp) = &mut i.modpack {
            mp.missing.retain(|m| !added.contains(&m.file_name));
        }
    })?;
    if !added.is_empty() {
        app.server.log(format!("[omg] [{id}] fichiers du modpack ajoutés : {}", added.join(", ")));
    }
    Ok(Json(json!({ "added": added, "unknown": unknown })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cf_loaders() {
        assert_eq!(parse_cf_loader("forge-47.2.0").unwrap(), (Kind::Forge, "47.2.0".into()));
        assert_eq!(parse_cf_loader("neoforge-21.1.77").unwrap(), (Kind::Neoforge, "21.1.77".into()));
        assert_eq!(parse_cf_loader("fabric-0.16.10").unwrap(), (Kind::Fabric, "0.16.10".into()));
        assert!(parse_cf_loader("neoforge-1.20.1-47.1.106").is_err());
        assert!(parse_cf_loader("liteloader-1").is_err());
    }

    #[test]
    fn culprits_from_crash() {
        let r = "x\n-- Mod loading issue for: statuseffectbars --\nDetails:\n\tCaused by 0: java.lang.BootstrapMethodError: java.lang.RuntimeException: Attempted to load class net/minecraft/client/gui/screens/Screen for invalid dist DEDICATED_SERVER\n\t\tat TRANSFORMER/statuseffectbars@1.0.2/com.x.StatusEffectBars.<init>(StatusEffectBars.java:20) ~[statuseffectbars-1.21.1-NeoForge-1.0.2.jar%23314!/:?]\n-- Mod loading issue for: other --\nDetails:\n\tsomething else [other-1.0.jar%231!/:?]\n";
        assert_eq!(client_only_culprits(r), vec!["statuseffectbars-1.21.1-NeoForge-1.0.2.jar"]);
        assert!(client_only_culprits("rien").is_empty());
    }

    #[test]
    fn pack_paths() {
        assert!(safe_pack_path("mods/.connector/temp/a$b.jar"));
        assert!(safe_pack_path("config/x.toml"));
        assert!(!safe_pack_path("../x"));
        assert!(!safe_pack_path("mods/../../x"));
        assert!(!safe_pack_path("/etc/passwd"));
        assert!(!safe_pack_path("mods//a.jar"));
    }

    #[test]
    fn neoforge_mapping() {
        assert_eq!(install::neoforge_mc("21.1.77").as_deref(), Some("1.21.1"));
        assert_eq!(install::neoforge_mc("21.0.167").as_deref(), Some("1.21"));
        assert_eq!(install::neoforge_mc("20.4.237").as_deref(), Some("1.20.4"));
        assert_eq!(install::neoforge_mc("26.3.0.115").as_deref(), Some("26.3"));
        assert_eq!(install::neoforge_mc("26.1.2.129").as_deref(), Some("26.1.2"));
        assert_eq!(install::neoforge_mc("26.1.0.19-beta").as_deref(), Some("26.1"));
        assert_eq!(install::neoforge_mc("0.25w14craftmine.3-beta"), None);
    }

    fn write_zip(path: &FsPath, entries: &[(&str, &[u8])]) {
        use std::io::Write;
        let mut w = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (n, d) in entries {
            w.start_file(*n, zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(d).unwrap();
        }
        w.finish().unwrap();
    }

    #[test]
    fn headers_and_overrides() {
        let base = std::env::temp_dir().join(format!("omg-mp-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let mr = base.join("a.mrpack");
        write_zip(
            &mr,
            &[
                (
                    "modrinth.index.json",
                    br#"{"formatVersion":1,"game":"minecraft","versionId":"1.2","name":"Pack","files":[],
                        "dependencies":{"minecraft":"1.21.1","neoforge":"21.1.77"}}"#,
                ),
                ("overrides/config/a.toml", b"a=1"),
                ("overrides/resourcepacks/r.zip", b"r"),
                ("client-overrides/options.txt", b"o"),
            ],
        );
        let h = read_header(&mr).unwrap();
        assert_eq!((h.format, h.kind, h.mc.as_str()), (Format::Modrinth, Kind::Neoforge, "1.21.1"));
        assert_eq!(h.loader_version.as_deref(), Some("21.1.77"));
        let (s, c) = (base.join("s"), base.join("s/client"));
        extract_prefix(&mr, "overrides", &[(s.clone(), &CLIENT_ONLY_TOP), (c.clone(), &[])]).unwrap();
        extract_prefix(&mr, "client-overrides", &[(c.clone(), &[])]).unwrap();
        assert!(s.join("config/a.toml").is_file());
        assert!(!s.join("resourcepacks").exists());
        assert!(c.join("resourcepacks/r.zip").is_file());
        assert!(c.join("options.txt").is_file());

        let cf = base.join("b.zip");
        write_zip(
            &cf,
            &[(
                "manifest.json",
                br#"{"minecraft":{"version":"1.20.1","modLoaders":[{"id":"forge-47.2.0","primary":true}]},
                    "name":"CF","version":"3","files":[{"projectID":1,"fileID":2,"required":true}],"overrides":"overrides"}"#,
            )],
        );
        let h = read_header(&cf).unwrap();
        assert_eq!((h.format, h.kind, h.mc.as_str()), (Format::CurseForge, Kind::Forge, "1.20.1"));
        let bad = base.join("c.zip");
        write_zip(&bad, &[("readme.txt", b"x")]);
        assert!(read_header(&bad).is_err());
        std::fs::remove_dir_all(base).unwrap();
    }
}
