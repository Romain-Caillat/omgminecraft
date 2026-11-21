use std::{net::SocketAddr, path::PathBuf};

use anyhow::{Context, Result};
use ipnet::IpNet;

/// Configuration, lue une fois au démarrage depuis l'environnement.
pub struct Config {
    /// Racine des données : `instances/`, `java/`, `tmp/`, `omg.json`.
    pub data_dir: PathBuf,
    pub listen: SocketAddr,
    /// Seules les IP de ces réseaux atteignent le panel. Par défaut : NetBird
    /// (100.64.0.0/10) et la boucle locale — le LAN n'y a pas accès.
    pub allowed: Vec<IpNet>,
    /// Port imposé à chaque instance au démarrage : c'est celui qu'ouvre la
    /// policy NetBird, une instance ne doit pas pouvoir en changer.
    pub game_port: u16,
    pub default_memory_mb: u32,
    /// Adresse que les joueurs tapent, affichée dans le panel.
    pub public_address: String,
    /// Sauvegardes : à monter à part des données, pour qu'un effacement de
    /// celles-ci ne les emporte pas.
    pub backup_dir: PathBuf,
    /// Heure locale de la sauvegarde automatique (0–23).
    pub backup_hour: u32,
    /// Durée de conservation des sauvegardes automatiques.
    pub backup_retention_days: u64,
    /// Place maximale des sauvegardes d'une instance.
    pub backup_max_bytes: u64,
    /// Clé de l'API CurseForge (console.curseforge.com) ; sans elle, seul
    /// Modrinth et l'import de fichier sont disponibles.
    pub curseforge_api_key: Option<String>,
    /// Écoute de la surface joueurs (téléchargement des packs clients).
    pub share_listen: SocketAddr,
    /// Base des liens donnés aux joueurs, telle qu'ils l'atteignent.
    pub share_url: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let var = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
        let allowed = var("OMG_ALLOWED_CIDRS", "100.64.0.0/10,127.0.0.0/8,::1/128")
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.parse::<IpNet>().with_context(|| format!("OMG_ALLOWED_CIDRS: {s}")))
            .collect::<Result<Vec<_>>>()?;
        let data_dir: PathBuf = var("OMG_DATA_DIR", "/srv/minecraft").into();
        let backup_hour: u32 = var("OMG_BACKUP_HOUR", "4").parse().context("OMG_BACKUP_HOUR")?;
        anyhow::ensure!(backup_hour < 24, "OMG_BACKUP_HOUR : entre 0 et 23");
        Ok(Self {
            backup_dir: std::env::var("OMG_BACKUP_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| data_dir.join("backups")),
            backup_hour,
            backup_max_bytes: {
                let gb: f64 = var("OMG_BACKUP_MAX_GB", "3").parse().context("OMG_BACKUP_MAX_GB")?;
                anyhow::ensure!(gb > 0.0, "OMG_BACKUP_MAX_GB doit être positif");
                (gb * 1024.0 * 1024.0 * 1024.0) as u64
            },
            backup_retention_days: var("OMG_BACKUP_RETENTION_DAYS", "14")
                .parse()
                .context("OMG_BACKUP_RETENTION_DAYS")?,
            curseforge_api_key: std::env::var("OMG_CURSEFORGE_API_KEY").ok().filter(|k| !k.trim().is_empty()),
            share_listen: var("OMG_SHARE_LISTEN", "0.0.0.0:8081").parse().context("OMG_SHARE_LISTEN")?,
            share_url: var("OMG_SHARE_URL", "http://localhost:8081"),
            data_dir,
            listen: var("OMG_LISTEN", "0.0.0.0:8080").parse().context("OMG_LISTEN")?,
            allowed,
            game_port: var("OMG_GAME_PORT", "25565").parse().context("OMG_GAME_PORT")?,
            default_memory_mb: var("OMG_DEFAULT_MEMORY_MB", "6144")
                .parse()
                .context("OMG_DEFAULT_MEMORY_MB")?,
            public_address: var("OMG_PUBLIC_ADDRESS", ""),
        })
    }
}
