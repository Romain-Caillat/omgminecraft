//! Une instance = un dossier `instances/<id>/` contenant un serveur Minecraft
//! complet (jar, `server.properties`, mods, mondes) et un `instance.json`.

use std::{
    io,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{
    app::App,
    error::{ApiResult, AppError},
};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Vanilla,
    Fabric,
    /// Serveur importé à la main : `jar` est lancé tel quel.
    Custom,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum InstallStatus {
    Installing,
    #[default]
    Ready,
    Error,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Instance {
    /// Nom du dossier, jamais stocké : il est relu à chaque chargement.
    #[serde(default, skip_deserializing)]
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub mc_version: String,
    #[serde(default)]
    pub loader_version: Option<String>,
    /// Fichier passé à `java -jar`, relatif au dossier de l'instance.
    pub jar: String,
    /// Version majeure de Java requise ; 0 tant que l'installation ne l'a pas lue.
    pub java_major: u32,
    pub memory_min_mb: u32,
    pub memory_max_mb: u32,
    #[serde(default)]
    pub jvm_args: Vec<String>,
    #[serde(default)]
    pub status: InstallStatus,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub created_at: u64,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = path.with_file_name(format!(
        ".{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file")
    ));
    std::fs::write(&tmp, data)?;
    std::fs::rename(tmp, path)
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 48
        && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !id.starts_with('-')
}

/// Nom de fichier ou de dossier fourni par l'utilisateur : un seul composant,
/// sans `..`, sans fichier caché, sans caractère de contrôle.
pub fn safe_name(name: &str) -> ApiResult<&str> {
    let ok = !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('.')
        && !name.contains(['/', '\\'])
        && !name.chars().any(char::is_control);
    if ok {
        Ok(name)
    } else {
        Err(AppError::bad(format!("nom invalide : {name:?}")))
    }
}

pub fn slugify(name: &str) -> String {
    let mut s = String::new();
    for c in name.to_lowercase().chars() {
        let c = match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            c => c,
        };
        if c.is_ascii_alphanumeric() {
            s.push(c);
        } else if !s.ends_with('-') {
            s.push('-');
        }
    }
    let s: String = s.trim_matches('-').chars().take(40).collect();
    let s = s.trim_end_matches('-').to_string();
    if s.is_empty() { "instance".into() } else { s }
}

impl App {
    pub fn load_instance(&self, id: &str) -> ApiResult<Instance> {
        if !valid_id(id) {
            return Err(AppError::bad("identifiant d'instance invalide"));
        }
        let path = self.instance_dir(id).join("instance.json");
        let raw = match std::fs::read(&path) {
            Ok(r) => r,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(AppError::not_found(format!("instance inconnue : {id}")))
            }
            Err(e) => return Err(e.into()),
        };
        let mut inst: Instance = serde_json::from_slice(&raw)
            .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        inst.id = id.to_string();
        Ok(inst)
    }

    pub fn save_instance(&self, inst: &Instance) -> ApiResult<()> {
        let path = self.instance_dir(&inst.id).join("instance.json");
        let data = serde_json::to_vec_pretty(inst)?;
        write_atomic(&path, &data)?;
        Ok(())
    }

    /// Applique `f` à l'instance relue sur disque puis la sauvegarde.
    pub fn update_instance(&self, id: &str, f: impl FnOnce(&mut Instance)) -> ApiResult<Instance> {
        let mut inst = self.load_instance(id)?;
        f(&mut inst);
        self.save_instance(&inst)?;
        Ok(inst)
    }

    pub fn list_instances(&self) -> ApiResult<Vec<Instance>> {
        let mut out = vec![];
        for entry in std::fs::read_dir(self.instances_dir())? {
            let entry = entry?;
            let Some(id) = entry.file_name().to_str().map(str::to_string) else { continue };
            if !valid_id(&id) || !entry.path().join("instance.json").exists() {
                continue;
            }
            match self.load_instance(&id) {
                Ok(i) => out.push(i),
                Err(e) => tracing::warn!("instance {id} illisible : {}", e.1),
            }
        }
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Ok(out)
    }

    pub fn unique_id(&self, name: &str) -> String {
        let base = slugify(name);
        let mut id = base.clone();
        let mut n = 2;
        while self.instance_dir(&id).exists() {
            id = format!("{base}-{n}");
            n += 1;
        }
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slugify("One Life — Été 1.21"), "one-life-ete-1-21");
        assert_eq!(slugify("///"), "instance");
        assert!(valid_id(&slugify("Vanilla 1.20.1")));
    }

    #[test]
    fn names() {
        assert!(safe_name("sodium.jar").is_ok());
        assert!(safe_name("../etc").is_err());
        assert!(safe_name("a/b").is_err());
        assert!(safe_name(".hidden").is_err());
        assert!(safe_name("").is_err());
    }
}
