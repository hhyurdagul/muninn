use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use muninn_vault::Vault;
use serde::{Deserialize, Serialize};

use crate::provider::ProviderKind;

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
    pub provider: ProviderKind,
    pub model: String,
    /// Endpoint of the `custom` provider; the others have fixed ones.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub base_url: String,
    /// API keys by provider id, so switching back and forth keeps them.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub api_keys: BTreeMap<String, String>,
    /// The single key of settings written before providers existed; moved
    /// into `api_keys` on load.
    #[serde(skip_serializing)]
    api_key: Option<String>,
    /// Gitignore-style patterns, relative to the vault root, for paths Muninn
    /// can neither see nor touch. Re-read on every turn.
    pub ignore: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            vault: None,
            provider: ProviderKind::default(),
            model: DEFAULT_MODEL.into(),
            base_url: String::new(),
            api_keys: BTreeMap::new(),
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
        let mut settings: Self =
            toml::from_str(&text).with_context(|| format!("invalid settings in {}", file.display()))?;
        settings.migrate();
        Ok(settings)
    }

    /// Older settings had one endpoint and key and no provider: pick the
    /// provider by endpoint, or `custom` for an unknown one.
    fn migrate(&mut self) {
        let Some(key) = self.api_key.take() else { return };
        let url = self.base_url.trim_end_matches('/');
        self.provider =
            ProviderKind::ALL.into_iter().find(|kind| kind.base_url() == Some(url)).unwrap_or(ProviderKind::Custom);
        if self.provider != ProviderKind::Custom {
            self.base_url.clear();
        }
        if !key.is_empty() {
            self.api_keys.entry(self.provider.id().into()).or_insert(key);
        }
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

    /// Where requests for the current provider go.
    pub fn endpoint(&self) -> &str {
        self.provider.base_url().unwrap_or(&self.base_url)
    }

    /// The key for the current provider.
    pub fn api_key(&self) -> Option<String> {
        self.key_for(self.provider)
    }

    /// `MUNINN_API_KEY` wins, then the stored key, then the provider's usual
    /// environment variable (such as `ANTHROPIC_API_KEY`).
    pub fn key_for(&self, provider: ProviderKind) -> Option<String> {
        let env = |name: &str| std::env::var(name).ok().filter(|k| !k.is_empty());
        env("MUNINN_API_KEY")
            .or_else(|| self.api_keys.get(provider.id()).cloned().filter(|k| !k.is_empty()))
            .or_else(|| provider.key_env().and_then(env))
    }

    /// Stores (or with an empty key, forgets) the key for a provider.
    pub fn set_key(&mut self, provider: ProviderKind, key: &str) {
        match key.trim() {
            "" => self.api_keys.remove(provider.id()),
            key => self.api_keys.insert(provider.id().into(), key.into()),
        };
    }

    /// The configured vault, with the ignore patterns applied.
    pub fn open_vault(&self) -> Result<Vault> {
        let vault = Vault::open(self.vault.as_ref().context("no vault configured; run `muninn init`")?)?;
        vault.set_ignore(&self.ignore)?;
        Ok(vault)
    }

    /// True once onboarding stored a usable vault and model.
    pub fn is_complete(&self) -> bool {
        self.vault.as_ref().is_some_and(|v| v.is_dir())
            && !self.model.is_empty()
            && !self.endpoint().is_empty()
            && (self.api_key().is_some() || !self.provider.needs_key())
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
        let mut settings = Settings { vault: Some(dir.path().into()), ..Settings::default() };
        settings.set_key(ProviderKind::OpenRouter, "k");
        settings.save(&paths).unwrap();
        assert_eq!(Settings::load(&paths).unwrap(), settings);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(paths.settings_file()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn single_key_settings_move_to_their_provider() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths { config_dir: dir.path().into(), data_dir: dir.path().into() };
        let legacy = |url: &str| {
            let text = format!("base_url = {url:?}\nmodel = \"m\"\napi_key = \"k\"\n");
            fs::write(paths.settings_file(), text).unwrap();
            Settings::load(&paths).unwrap()
        };
        let openrouter = legacy("https://openrouter.ai/api/v1/");
        assert_eq!(openrouter.provider, ProviderKind::OpenRouter);
        assert_eq!(openrouter.endpoint(), "https://openrouter.ai/api/v1");
        assert_eq!(openrouter.api_keys.get("openrouter").map(String::as_str), Some("k"));
        let local = legacy("http://localhost:11434/v1");
        assert_eq!((local.provider, local.endpoint()), (ProviderKind::Custom, "http://localhost:11434/v1"));
        local.save(&paths).unwrap();
        assert!(!fs::read_to_string(paths.settings_file()).unwrap().contains("api_key ="));
        assert_eq!(Settings::load(&paths).unwrap(), local);
    }
}
