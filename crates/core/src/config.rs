use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use muninn_vault::Vault;
use serde::{Deserialize, Serialize};

pub const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";
pub const DEFAULT_MODEL: &str = "anthropic/claude-sonnet-4.5";

/// Where Muninn keeps its own files, all outside the vault.
/// `MUNINN_CONFIG_DIR` and `MUNINN_DATA_DIR` override the defaults.
#[derive(Debug, Clone)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl Paths {
    pub fn from_env() -> Self {
        let dir = |var: &str, base: Option<PathBuf>| {
            std::env::var_os(var)
                .map(PathBuf::from)
                .unwrap_or_else(|| base.unwrap_or_else(|| PathBuf::from(".")).join("muninn"))
        };
        Self {
            config_dir: dir("MUNINN_CONFIG_DIR", dirs::config_dir()),
            data_dir: dir("MUNINN_DATA_DIR", dirs::data_dir()),
        }
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn conversations_dir(&self) -> PathBuf {
        self.data_dir.join("conversations")
    }

    pub fn journal_file(&self) -> PathBuf {
        self.data_dir.join("changes.jsonl")
    }
}

/// Muninn's own settings: the vault path, the model, credentials, and which
/// vault paths Muninn may never touch. Everything else (rules, structure)
/// lives in the vault.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub vault: Option<PathBuf>,
    pub base_url: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Gitignore-style patterns, relative to the vault root, for paths Muninn
    /// can neither see nor touch. Re-read on every turn.
    pub ignore: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            vault: None,
            base_url: DEFAULT_BASE_URL.into(),
            model: DEFAULT_MODEL.into(),
            api_key: None,
            ignore: vec![".git/".into()],
        }
    }
}

impl Settings {
    pub fn load(paths: &Paths) -> Result<Self> {
        let file = paths.settings_file();
        if !file.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&file)?;
        toml::from_str(&text).with_context(|| format!("invalid settings in {}", file.display()))
    }

    /// Writes the settings file, readable only by the user (it holds the key).
    pub fn save(&self, paths: &Paths) -> Result<()> {
        fs::create_dir_all(&paths.config_dir)?;
        let file = paths.settings_file();
        fs::write(&file, toml::to_string_pretty(self)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    /// `MUNINN_API_KEY` wins over the stored key.
    pub fn api_key(&self) -> Option<String> {
        std::env::var("MUNINN_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())
            .or_else(|| self.api_key.clone().filter(|k| !k.is_empty()))
    }

    /// The configured vault, with the ignore patterns applied.
    pub fn open_vault(&self) -> Result<Vault> {
        let vault = Vault::open(self.vault.as_ref().context("no vault configured; run `muninn init`")?)?;
        vault.set_ignore(&self.ignore)?;
        Ok(vault)
    }

    /// True once onboarding stored a usable vault and key.
    pub fn is_complete(&self) -> bool {
        self.vault.as_ref().is_some_and(|v| v.is_dir()) && self.api_key().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_and_restricts_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths { config_dir: dir.path().join("cfg"), data_dir: dir.path().join("data") };
        assert_eq!(Settings::load(&paths).unwrap(), Settings::default());
        let settings = Settings { vault: Some(dir.path().into()), api_key: Some("k".into()), ..Settings::default() };
        settings.save(&paths).unwrap();
        assert_eq!(Settings::load(&paths).unwrap(), settings);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(paths.settings_file()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
