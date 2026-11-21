//! Téléchargements : jars serveur (Mojang, Paper, Fabric, Quilt, Forge,
//! NeoForge) et JRE (Adoptium).

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
const QUILT_META: &str = "https://meta.quiltmc.org/v3";
const PAPER_API: &str = "https://fill.papermc.io/v3/projects/paper";
const FORGE_MAVEN: &str = "https://maven.minecraftforge.net/net/minecraftforge/forge";
const FORGE_FILES: &str = "https://files.minecraftforge.net/net/minecraftforge/forge";
const NEOFORGE_MAVEN: &str = "https://maven.neoforged.net/releases/net/neoforged/neoforge";
const NEOFORGE_VERSIONS: &str = "https://maven.neoforged.net/api/maven/versions/releases/net/neoforged/neoforge";

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

async fn get_json<T: serde::de::DeserializeOwned>(http: &reqwest::Client, url: &str) -> Result<T> {
    http.get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .with_context(|| format!("réponse illisible de {url}"))
}

async fn manifest(http: &reqwest::Client) -> Result<Manifest> {
    get_json(http, MOJANG_MANIFEST).await
}

fn release_or_snapshot(v: &str) -> String {
    let pre = v.contains('-') || v.contains("pre") || v.contains("rc") || v.contains('w');
    if pre { "snapshot" } else { "release" }.into()
}

/// Version de Minecraft visée par une version de NeoForge.
/// - ancien schéma `21.1.77` → 1.21.1 (`20.2.x` → 1.20.2, `21.0.x` → 1.21) ;
/// - nouveau schéma `26.3.0.115` → 26.3 (`26.1.2.129` → 26.1.2).
pub fn neoforge_mc(v: &str) -> Option<String> {
    let base = v.split('-').next()?;
    let p: Vec<&str> = base.split('.').collect();
    let major: u32 = p.first()?.parse().ok()?;
    match (major, p.len()) {
        (20..=25, 3) => Some(if p[1] == "0" { format!("1.{}", p[0]) } else { format!("1.{}.{}", p[0], p[1]) }),
        (26.., 4) => Some(if p[2] == "0" { format!("{}.{}", p[0], p[1]) } else { format!("{}.{}.{}", p[0], p[1], p[2]) }),
        _ => None,
    }
}

async fn neoforge_versions(http: &reqwest::Client) -> Result<Vec<String>> {
    #[derive(Deserialize)]
    struct R {
        versions: Vec<String>,
    }
    Ok(get_json::<R>(http, NEOFORGE_VERSIONS).await?.versions)
}

/// Dernière NeoForge pour `mc` : stable de préférence, bêta sinon.
async fn neoforge_for(http: &reqwest::Client, mc: &str) -> Result<String> {
    let all: Vec<String> = neoforge_versions(http)
        .await?
        .into_iter()
        .filter(|v| neoforge_mc(v).as_deref() == Some(mc))
        .collect();
    all.iter()
        .rev()
        .find(|v| !v.contains('-'))
        .or(all.last())
        .cloned()
        .ok_or_else(|| anyhow!("pas de NeoForge pour Minecraft {mc}"))
}

/// Forge recommandée pour `mc`, sinon la plus récente (`66.0.3`, sans le préfixe mc).
async fn forge_for(http: &reqwest::Client, mc: &str) -> Result<String> {
    #[derive(Deserialize)]
    struct Promos {
        promos: std::collections::HashMap<String, String>,
    }
    let p: Promos = get_json(http, &format!("{FORGE_FILES}/promotions_slim.json")).await?;
    p.promos
        .get(&format!("{mc}-recommended"))
        .or_else(|| p.promos.get(&format!("{mc}-latest")))
        .cloned()
        .ok_or_else(|| anyhow!("pas de Forge pour Minecraft {mc}"))
}

async fn quilt_loader_latest(http: &reqwest::Client) -> Result<String> {
    #[derive(Deserialize)]
    struct L {
        version: String,
    }
    let list: Vec<L> = get_json(http, &format!("{QUILT_META}/versions/loader")).await?;
    list.into_iter()
        .map(|l| l.version)
        .find(|v| !v.contains('-'))
        .ok_or_else(|| anyhow!("aucune version stable de Quilt"))
}

async fn quilt_installer(http: &reqwest::Client) -> Result<String> {
    #[derive(Deserialize)]
    struct I {
        url: String,
    }
    let list: Vec<I> = get_json(http, &format!("{QUILT_META}/versions/installer")).await?;
    list.into_iter().next().map(|i| i.url).ok_or_else(|| anyhow!("installeur Quilt introuvable"))
}

pub async fn versions(http: &reqwest::Client, kind: Kind) -> Result<Vec<GameVersion>> {
    match kind {
        Kind::Vanilla | Kind::Custom => Ok(manifest(http)
            .await?
            .versions
            .into_iter()
            .map(|v| GameVersion { id: v.id, kind: v.kind })
            .collect()),
        Kind::Fabric | Kind::Quilt => {
            let url = if kind == Kind::Fabric {
                format!("{FABRIC_META}/versions/game")
            } else {
                format!("{QUILT_META}/versions/game")
            };
            let list: Vec<FabricEntry> = get_json(http, &url).await?;
            Ok(list
                .into_iter()
                .map(|v| GameVersion {
                    id: v.version,
                    kind: if v.stable { "release" } else { "snapshot" }.into(),
                })
                .collect())
        }
        Kind::Paper => {
            #[derive(Deserialize)]
            struct P {
                versions: indexmap_like::Ordered,
            }
            let p: P = get_json(http, PAPER_API).await?;
            Ok(p.versions
                .0
                .into_iter()
                .flat_map(|(_, list)| list)
                .map(|id| GameVersion { kind: release_or_snapshot(&id), id })
                .collect())
        }
        Kind::Forge => {
            let meta: indexmap_like::Ordered =
                get_json(http, &format!("{FORGE_FILES}/maven-metadata.json")).await?;
            Ok(meta
                .0
                .into_iter()
                .rev()
                .map(|(id, _)| GameVersion { kind: release_or_snapshot(&id.replace('_', "-")), id })
                .collect())
        }
        Kind::Neoforge => {
            let mut seen: Vec<String> = vec![];
            for v in neoforge_versions(http).await?.iter().rev() {
                if let Some(mc) = neoforge_mc(v) {
                    if !seen.contains(&mc) {
                        seen.push(mc);
                    }
                }
            }
            Ok(seen.into_iter().map(|id| GameVersion { kind: "release".into(), id }).collect())
        }
    }
}

/// Objet JSON dont l'ordre des clés compte (versions Paper, Forge).
mod indexmap_like {
    use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};

    pub struct Ordered(pub Vec<(String, Vec<String>)>);

    impl<'de> Deserialize<'de> for Ordered {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct V;
            impl<'de> Visitor<'de> for V {
                type Value = Ordered;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("un objet de listes")
                }
                fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Ordered, A::Error> {
                    let mut out = vec![];
                    while let Some((k, v)) = m.next_entry::<String, Vec<String>>()? {
                        out.push((k, v));
                    }
                    Ok(Ordered(out))
                }
            }
            d.deserialize_map(V)
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
    let json: VersionJson = get_json(http, &v.url).await?;
    let server = json
        .downloads
        .and_then(|d| d.server)
        .ok_or_else(|| anyhow!("Mojang ne publie pas de serveur pour {mc}"))?;
    // Les versions antérieures au champ `javaVersion` tournent sous Java 8.
    let java = json.java_version.map(|j| j.major_version).unwrap_or(8);
    Ok((server, java))
}

async fn fabric_latest(http: &reqwest::Client, what: &str) -> Result<String> {
    let list: Vec<FabricEntry> = get_json(http, &format!("{FABRIC_META}/versions/{what}")).await?;
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

/// Installation d'une instance fraîchement créée, en tâche de fond. `pack` :
/// modpack à poser par-dessus le loader.
pub async fn install_instance(app: Arc<App>, inst: Instance, pack: Option<crate::modpacks::PackSource>) {
    let id = inst.id.clone();
    let res = async {
        do_install(&app, &inst).await?;
        if let Some(pack) = pack {
            crate::modpacks::install_pack(&app, &id, pack).await?;
        }
        let inst = app.load_instance(&id).map_err(|e| anyhow!(e.1))?;
        let java = ensure_java(&app, inst.java_major).await?;
        init_settings(&app, &inst, &java).await
    }
    .await;
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

/// Lance un installeur Forge/NeoForge/Quilt dans `dir`.
async fn run_installer(app: &App, java: &Path, dir: &Path, args: &[&str]) -> Result<()> {
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(900),
        tokio::process::Command::new(java)
            .current_dir(dir)
            .args(args)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| anyhow!("l'installeur n'a pas fini en 15 minutes"))??;
    if !out.status.success() {
        let tail: String = String::from_utf8_lossy(&out.stdout)
            .lines()
            .chain(String::from_utf8_lossy(&out.stderr).lines())
            .filter(|l| !l.trim().is_empty())
            .collect::<Vec<_>>()
            .iter()
            .rev()
            .take(5)
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join(" | ");
        bail!("l'installeur a échoué ({}) : {tail}", out.status);
    }
    let _ = app;
    Ok(())
}

/// Cible de lancement laissée par l'installeur Forge/NeoForge : le fichier
/// d'arguments des versions récentes, ou le jar des anciennes.
fn forge_launch_target(dir: &Path, group: &str, version_dir: &str) -> Result<String> {
    let args = format!("libraries/{group}/{version_dir}/unix_args.txt");
    if dir.join(&args).is_file() {
        return Ok(args);
    }
    for e in std::fs::read_dir(dir)?.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if (n.starts_with("forge-") || n.starts_with("neoforge-"))
            && n.ends_with(".jar")
            && !n.contains("installer")
        {
            return Ok(n);
        }
    }
    bail!("l'installeur n'a laissé ni unix_args.txt ni jar serveur")
}

async fn do_install(app: &Arc<App>, inst: &Instance) -> Result<()> {
    let dir = app.instance_dir(&inst.id);
    let log = |m: String| app.server.log(format!("[omg] [{}] {m}", inst.id));
    let mc = inst.mc_version.as_str();
    let (server, java) = vanilla_details(&app.http, mc).await?;
    app.update_instance(&inst.id, |i| i.java_major = java).map_err(|e| anyhow!(e.1))?;
    let set = |f: Box<dyn FnOnce(&mut Instance) + Send>| app.update_instance(&inst.id, f).map_err(|e| anyhow!(e.1));
    match inst.kind {
        Kind::Vanilla => {
            log(format!("téléchargement du serveur vanilla {mc}"));
            download(&app.http, &server.url, &dir.join(&inst.jar), Some(&server.sha1)).await?;
        }
        Kind::Fabric => {
            let loader = match &inst.loader_version {
                Some(v) => v.clone(),
                None => fabric_latest(&app.http, "loader").await?,
            };
            let installer = fabric_latest(&app.http, "installer").await?;
            log(format!("téléchargement de Fabric {loader} pour {mc}"));
            let url = format!("{FABRIC_META}/versions/loader/{mc}/{loader}/{installer}/server/jar");
            download(&app.http, &url, &dir.join(&inst.jar), None).await?;
            // Le lanceur Fabric télécharge lui-même le jar vanilla au premier
            // démarrage ; on le dépose d'avance pour que ce soit vérifié et immédiat.
            download(&app.http, &server.url, &dir.join("server.jar"), Some(&server.sha1)).await?;
            set(Box::new(move |i| i.loader_version = Some(loader)))?;
        }
        Kind::Paper => {
            #[derive(Deserialize)]
            struct Build {
                id: u64,
                downloads: std::collections::HashMap<String, PaperDl>,
            }
            #[derive(Deserialize)]
            struct PaperDl {
                url: String,
            }
            let b: Build = get_json(&app.http, &format!("{PAPER_API}/versions/{mc}/builds/latest")).await?;
            let dl = b
                .downloads
                .get("server:default")
                .ok_or_else(|| anyhow!("Paper ne publie pas de serveur pour {mc}"))?;
            log(format!("téléchargement de Paper {mc} (build {})", b.id));
            download(&app.http, &dl.url, &dir.join(&inst.jar), None).await?;
            let build = b.id.to_string();
            set(Box::new(move |i| i.loader_version = Some(build)))?;
        }
        Kind::Quilt => {
            let loader = match &inst.loader_version {
                Some(v) => v.clone(),
                None => quilt_loader_latest(&app.http).await?,
            };
            let installer = dir.join("quilt-installer.jar");
            download(&app.http, &quilt_installer(&app.http).await?, &installer, None).await?;
            let java_bin = ensure_java(app, java.max(17)).await?;
            log(format!("installation de Quilt {loader} pour {mc}"));
            run_installer(
                app,
                &java_bin,
                &dir,
                &["-jar", "quilt-installer.jar", "install", "server", mc, &loader, "--download-server", "--install-dir=."],
            )
            .await?;
            let _ = tokio::fs::remove_file(&installer).await;
            set(Box::new(move |i| i.loader_version = Some(loader)))?;
        }
        Kind::Forge | Kind::Neoforge => {
            let forge = inst.kind == Kind::Forge;
            let loader = match &inst.loader_version {
                Some(v) => v.clone(),
                None if forge => forge_for(&app.http, mc).await?,
                None => neoforge_for(&app.http, mc).await?,
            };
            let (url, group, vdir) = if forge {
                let full = format!("{mc}-{loader}");
                (
                    format!("{FORGE_MAVEN}/{full}/forge-{full}-installer.jar"),
                    "net/minecraftforge/forge",
                    full,
                )
            } else {
                (
                    format!("{NEOFORGE_MAVEN}/{loader}/neoforge-{loader}-installer.jar"),
                    "net/neoforged/neoforge",
                    loader.clone(),
                )
            };
            let name = if forge { "Forge" } else { "NeoForge" };
            log(format!("installation de {name} {loader} pour {mc}"));
            let installer = dir.join("installer.jar");
            download(&app.http, &url, &installer, None).await?;
            let java_bin = ensure_java(app, java).await?;
            run_installer(app, &java_bin, &dir, &["-jar", "installer.jar", "--installServer"]).await?;
            let _ = tokio::fs::remove_file(&installer).await;
            let _ = tokio::fs::remove_file(dir.join("installer.jar.log")).await;
            let target = forge_launch_target(&dir, group, &vdir)?;
            set(Box::new(move |i| {
                i.loader_version = Some(loader);
                i.jar = target;
            }))?;
        }
        Kind::Custom => bail!("une instance personnalisée ne s'installe pas"),
    }
    Ok(())
}

/// Arguments de lancement après les options de la JVM : `-jar x.jar`, ou
/// `@fichier` pour Forge/NeoForge récents.
pub fn launch_target(jar: &str) -> Vec<String> {
    if jar.ends_with(".txt") {
        vec![format!("@{jar}")]
    } else {
        vec!["-jar".into(), jar.into()]
    }
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
        .arg("-Xmx1024M")
        .args(launch_target(&inst.jar))
        .args(["--initSettings", "nogui"])
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
    app.enforce_props(&mut props);
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
