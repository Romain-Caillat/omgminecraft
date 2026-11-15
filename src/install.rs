//! Téléchargements : jars serveur (Mojang, Fabric) et JRE (Adoptium).

use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use tokio::io::AsyncWriteExt;

use crate::{
    app::App,
    instances::{InstallStatus, Instance, Kind},
};

const MOJANG_MANIFEST: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const FABRIC_META: &str = "https://meta.fabricmc.net/v2";

#[derive(Serialize, Clone)]
pub struct GameVersion {
    pub id: String,
    /// `release`, `snapshot`, `old_beta`, `old_alpha`.
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Deserialize)]
struct Manifest {
    versions: Vec<ManifestVersion>,
}

#[derive(Deserialize)]
struct ManifestVersion {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    url: String,
}

#[derive(Deserialize)]
struct VersionJson {
    downloads: Option<Downloads>,
    #[serde(rename = "javaVersion")]
    java_version: Option<JavaVersion>,
}

#[derive(Deserialize)]
struct Downloads {
    server: Option<Download>,
}

#[derive(Deserialize)]
struct Download {
    url: String,
    sha1: String,
}

#[derive(Deserialize)]
struct JavaVersion {
    #[serde(rename = "majorVersion")]
    major_version: u32,
}

#[derive(Deserialize)]
struct FabricEntry {
    version: String,
    stable: bool,
}

async fn manifest(http: &reqwest::Client) -> Result<Manifest> {
    Ok(http.get(MOJANG_MANIFEST).send().await?.error_for_status()?.json().await?)
}

pub async fn versions(http: &reqwest::Client, kind: Kind) -> Result<Vec<GameVersion>> {
    match kind {
        Kind::Vanilla | Kind::Custom => Ok(manifest(http)
            .await?
            .versions
            .into_iter()
            .map(|v| GameVersion { id: v.id, kind: v.kind })
            .collect()),
        Kind::Fabric => {
            let list: Vec<FabricEntry> = http
                .get(format!("{FABRIC_META}/versions/game"))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            Ok(list
                .into_iter()
                .map(|v| GameVersion {
                    id: v.version,
                    kind: if v.stable { "release" } else { "snapshot" }.into(),
                })
                .collect())
        }
    }
}

/// URL et empreinte du jar serveur vanilla, et la version de Java qu'il exige.
async fn vanilla_details(http: &reqwest::Client, mc: &str) -> Result<(Download, u32)> {
    let m = manifest(http).await?;
    let v = m
        .versions
        .into_iter()
        .find(|v| v.id == mc)
        .ok_or_else(|| anyhow!("version inconnue chez Mojang : {mc}"))?;
    let json: VersionJson = http.get(&v.url).send().await?.error_for_status()?.json().await?;
    let server = json
        .downloads
        .and_then(|d| d.server)
        .ok_or_else(|| anyhow!("Mojang ne publie pas de serveur pour {mc}"))?;
    // Les versions antérieures au champ `javaVersion` tournent sous Java 8.
    let java = json.java_version.map(|j| j.major_version).unwrap_or(8);
    Ok((server, java))
}

async fn fabric_latest(http: &reqwest::Client, what: &str) -> Result<String> {
    let list: Vec<FabricEntry> = http
        .get(format!("{FABRIC_META}/versions/{what}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    list.into_iter()
        .find(|e| e.stable)
        .map(|e| e.version)
        .ok_or_else(|| anyhow!("aucune version stable de Fabric {what}"))
}

/// Télécharge en flux vers `dest` (via un `.part`), en vérifiant le SHA-1 s'il est connu.
pub async fn download(http: &reqwest::Client, url: &str, dest: &Path, sha1: Option<&str>) -> Result<()> {
    let resp = http.get(url).send().await?.error_for_status()?;
    let part = dest.with_file_name(format!(
        "{}.part",
        dest.file_name().and_then(|n| n.to_str()).unwrap_or("download")
    ));
    let mut file = tokio::fs::File::create(&part).await?;
    let mut hasher = Sha1::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    drop(file);
    if let Some(expected) = sha1 {
        let got = hex::encode(hasher.finalize());
        if !got.eq_ignore_ascii_case(expected) {
            let _ = tokio::fs::remove_file(&part).await;
            bail!("SHA-1 incorrect pour {url} : {got} au lieu de {expected}");
        }
    }
    tokio::fs::rename(&part, dest).await?;
    Ok(())
}

/// Installation d'une instance fraîchement créée, en tâche de fond.
pub async fn install_instance(app: Arc<App>, inst: Instance) {
    let id = inst.id.clone();
    let res = do_install(&app, &inst).await;
    let log = |m: String| app.server.log(format!("[omg] [{id}] {m}"));
    match &res {
        Ok(()) => log("installation terminée".into()),
        Err(e) => log(format!("échec de l'installation : {e:#}")),
    }
    let _ = app.update_instance(&id, |i| match res {
        Ok(()) => {
            i.status = InstallStatus::Ready;
            i.error = None;
        }
        Err(e) => {
            i.status = InstallStatus::Error;
            i.error = Some(format!("{e:#}"));
        }
    });
}

async fn do_install(app: &Arc<App>, inst: &Instance) -> Result<()> {
    let dir = app.instance_dir(&inst.id);
    let log = |m: String| app.server.log(format!("[omg] [{}] {m}", inst.id));
    let (server, java) = vanilla_details(&app.http, &inst.mc_version).await?;
    match inst.kind {
        Kind::Vanilla => {
            log(format!("téléchargement du serveur vanilla {}", inst.mc_version));
            download(&app.http, &server.url, &dir.join(&inst.jar), Some(&server.sha1)).await?;
        }
        Kind::Fabric => {
            let loader = fabric_latest(&app.http, "loader").await?;
            let installer = fabric_latest(&app.http, "installer").await?;
            log(format!("téléchargement de Fabric {loader} pour {}", inst.mc_version));
            let url = format!(
                "{FABRIC_META}/versions/loader/{}/{loader}/{installer}/server/jar",
                inst.mc_version
            );
            download(&app.http, &url, &dir.join(&inst.jar), None).await?;
            // Le lanceur Fabric télécharge lui-même le jar vanilla au premier
            // démarrage ; on le dépose d'avance pour que ce soit vérifié et immédiat.
            download(&app.http, &server.url, &dir.join("server.jar"), Some(&server.sha1)).await?;
            app.update_instance(&inst.id, |i| i.loader_version = Some(loader))
                .map_err(|e| anyhow!(e.1))?;
        }
        Kind::Custom => bail!("une instance personnalisée ne s'installe pas"),
    }
    app.update_instance(&inst.id, |i| i.java_major = java).map_err(|e| anyhow!(e.1))?;
    let java = ensure_java(app, java).await?;
    init_settings(app, inst, &java).await?;
    Ok(())
}

/// Fait écrire au serveur son `server.properties` complet (toutes les clés de
/// la version, avec leurs valeurs par défaut) sans le démarrer : sinon le
/// fichier n'existe qu'après un premier lancement.
pub async fn init_settings(app: &App, inst: &Instance, java: &Path) -> Result<()> {
    let dir = app.instance_dir(&inst.id);
    app.server.log(format!("[omg] [{}] génération de server.properties", inst.id));
    // Sans EULA acceptée, une version trop ancienne pour connaître
    // `--initSettings` s'arrête d'elle-même au lieu de démarrer pour de bon.
    let eula = dir.join("eula.txt");
    let _ = tokio::fs::remove_file(&eula).await;
    let mut cmd = tokio::process::Command::new(java);
    cmd.current_dir(&dir)
        .args(["-Xmx1024M", "-jar", &inst.jar, "--initSettings", "nogui"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let res = tokio::time::timeout(std::time::Duration::from_secs(300), cmd.status()).await;
    tokio::fs::write(&eula, "eula=true\n").await?;
    match res {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => return Err(e).context("lancement de java"),
        Err(_) => bail!("le serveur n'a pas généré sa configuration en 5 minutes"),
    }
    let path = dir.join("server.properties");
    let mut props = crate::properties::Properties::load(&path)?;
    if props.get("motd").is_none_or(|m| m == "A Minecraft Server") {
        props.set("motd", &inst.name);
    }
    app.enforce_network(&mut props);
    props.save(&path)?;
    Ok(())
}

fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        _ => "x64",
    }
}

fn system_java(major: u32) -> Option<PathBuf> {
    let a = if arch() == "x64" { "amd64" } else { "arm64" };
    ["jre", "jdk"]
        .iter()
        .map(|k| PathBuf::from(format!("/usr/lib/jvm/temurin-{major}-{k}-{a}/bin/java")))
        .find(|p| p.exists())
}

/// Chemin d'un `java` de la version demandée : celui du système s'il existe,
/// sinon un JRE Temurin téléchargé une fois dans `java/<major>/`.
pub async fn ensure_java(app: &App, major: u32) -> Result<PathBuf> {
    if major == 0 {
        bail!("version de Java inconnue pour cette instance");
    }
    if let Some(p) = system_java(major) {
        return Ok(p);
    }
    let home = app.java_dir().join(major.to_string());
    let bin = home.join("bin/java");
    if bin.exists() {
        return Ok(bin);
    }
    let _guard = app.java_lock.lock().await;
    if bin.exists() {
        return Ok(bin);
    }
    app.server.log(format!("[omg] téléchargement de Java {major} (Temurin)"));
    let archive = app.tmp_path("tar.gz");
    let mut last_err = None;
    for image in ["jre", "jdk"] {
        let url = format!(
            "https://api.adoptium.net/v3/binary/latest/{major}/ga/linux/{}/{image}/hotspot/normal/eclipse",
            arch()
        );
        match download(&app.http, &url, &archive, None).await {
            Ok(()) => {
                last_err = None;
                break;
            }
            Err(e) => last_err = Some(e),
        }
    }
    if let Some(e) = last_err {
        return Err(e.context(format!("Java {major} introuvable chez Adoptium")));
    }
    let staging = app.java_dir().join(format!(".{major}.staging"));
    let (a, s) = (archive.clone(), staging.clone());
    tokio::task::spawn_blocking(move || extract_tgz_strip(&a, &s)).await??;
    let _ = tokio::fs::remove_file(&archive).await;
    if !staging.join("bin/java").exists() {
        bail!("archive Java {major} inattendue : pas de bin/java");
    }
    tokio::fs::rename(&staging, &home).await?;
    app.server.log(format!("[omg] Java {major} installé"));
    Ok(bin)
}

/// Extrait une archive `.tar.gz` en retirant son dossier racine.
fn extract_tgz_strip(archive: &Path, dest: &Path) -> Result<()> {
    if dest.exists() {
        std::fs::remove_dir_all(dest)?;
    }
    std::fs::create_dir_all(dest)?;
    let gz = flate2::read::GzDecoder::new(std::fs::File::open(archive)?);
    let mut ar = tar::Archive::new(gz);
    for entry in ar.entries()? {
        let mut e = entry?;
        let path = e.path()?.into_owned();
        let rel: PathBuf = path.components().skip(1).collect();
        if rel.as_os_str().is_empty() || rel.components().any(|c| !matches!(c, Component::Normal(_))) {
            continue;
        }
        let out = dest.join(&rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        e.unpack(&out).with_context(|| format!("extraction de {}", rel.display()))?;
    }
    Ok(())
}
