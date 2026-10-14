//! What the app remembers between launches: the chosen model, effort and approval mode, and the
//! folders added as projects. Kept in `app.json` in rozu's config dir.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// `read-only`, `auto` or `full` (the `rozu chat --approval` names); `None` means auto.
    pub approval: Option<String>,
    pub projects: Vec<PathBuf>,
}

impl Settings {
    fn path() -> Option<PathBuf> {
        let dirs = directories::ProjectDirs::from("dev", "rozu", "rozu")?;
        Some(dirs.config_dir().join("app.json"))
    }

    pub fn load() -> Self {
        Self::path().map_or_else(Self::default, |p| Self::read(&p))
    }

    pub fn save(&self) {
        if let Some(p) = Self::path() {
            let _ = self.write(&p);
        }
    }

    fn read(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn write(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)
    }

    /// Adds a project folder once; returns whether it was new.
    pub fn add_project(&mut self, root: PathBuf) -> bool {
        let new = !self.projects.contains(&root);
        if new {
            self.projects.push(root);
        }
        new
    }

    pub fn remove_project(&mut self, root: &Path) {
        self.projects.retain(|p| p != root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_tolerates_junk() {
        let dir = std::env::temp_dir().join(format!("rozu-app-settings-{}", std::process::id()));
        let path = dir.join("app.json");
        assert_eq!(Settings::read(&path), Settings::default());
        let mut s = Settings {
            provider: Some("claude-cli".into()),
            model: Some("opus".into()),
            effort: Some("high".into()),
            approval: Some("read-only".into()),
            ..Default::default()
        };
        assert!(s.add_project("/a".into()));
        assert!(!s.add_project("/a".into()));
        s.add_project("/b".into());
        s.remove_project(Path::new("/a"));
        s.write(&path).unwrap();
        assert_eq!(Settings::read(&path), s);
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(Settings::read(&path), Settings::default());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
