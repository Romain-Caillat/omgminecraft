//! `server.properties`, édité en place : l'ordre et les commentaires sont
//! conservés, et les valeurs restent brutes (échappements Java compris, ex.
//! `minecraft\:normal`) pour que la relecture soit exacte.

use std::{io, path::Path};

enum Line {
    Raw(String),
    Kv(String, String),
}

pub struct Properties {
    lines: Vec<Line>,
}

impl Properties {
    pub fn load(path: &Path) -> io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => Ok(Self::parse(&s)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self { lines: vec![] }),
            Err(e) => Err(e),
        }
    }

    fn parse(s: &str) -> Self {
        let lines = s
            .lines()
            .map(|l| {
                let t = l.trim_start();
                if t.is_empty() || t.starts_with('#') || t.starts_with('!') {
                    Line::Raw(l.to_string())
                } else {
                    match t.split_once('=') {
                        Some((k, v)) => Line::Kv(k.trim().to_string(), v.to_string()),
                        None => Line::Kv(t.trim().to_string(), String::new()),
                    }
                }
            })
            .collect();
        Self { lines }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.lines.iter().find_map(|l| match l {
            Line::Kv(k, v) if k == key => Some(v.as_str()),
            _ => None,
        })
    }

    pub fn set(&mut self, key: &str, value: &str) {
        for l in &mut self.lines {
            if let Line::Kv(k, v) = l {
                if k == key {
                    *v = value.to_string();
                    return;
                }
            }
        }
        self.lines.push(Line::Kv(key.to_string(), value.to_string()));
    }

    #[allow(dead_code)]
    pub fn entries(&self) -> Vec<(String, String)> {
        self.lines
            .iter()
            .filter_map(|l| match l {
                Line::Kv(k, v) => Some((k.clone(), v.clone())),
                Line::Raw(_) => None,
            })
            .collect()
    }

    /// Remplace l'ensemble des clés : celles absentes de `entries` disparaissent.
    #[allow(dead_code)]
    pub fn replace_all(&mut self, entries: &[(String, String)]) {
        self.lines.retain(|l| match l {
            Line::Kv(k, _) => entries.iter().any(|(ek, _)| ek == k),
            Line::Raw(_) => true,
        });
        for (k, v) in entries {
            self.set(k, v);
        }
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let mut out = String::new();
        for l in &self.lines {
            match l {
                Line::Raw(s) => out.push_str(s),
                Line::Kv(k, v) => {
                    out.push_str(k);
                    out.push('=');
                    out.push_str(v);
                }
            }
            out.push('\n');
        }
        crate::instances::write_atomic(path, out.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_keeps_comments_and_escapes() {
        let src = "#Minecraft server properties\nlevel-type=minecraft\\:normal\nmotd=A=B\n";
        let mut p = Properties::parse(src);
        assert_eq!(p.get("motd"), Some("A=B"));
        assert_eq!(p.get("level-type"), Some("minecraft\\:normal"));
        p.set("server-port", "25565");
        p.replace_all(&[
            ("motd".into(), "Salut".into()),
            ("server-port".into(), "25565".into()),
        ]);
        assert!(p.get("level-type").is_none());
        assert_eq!(p.entries().len(), 2);
    }
}
