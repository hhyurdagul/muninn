//! The model providers Muninn can talk to, and connecting to the configured one.

use std::sync::Arc;

use anyhow::{Context, Result};
use muninn_ai::{Anthropic, OpenAiCompatible, Provider, ANTHROPIC_BASE_URL};
use serde::{Deserialize, Serialize};

use crate::config::Settings;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    #[default]
    OpenRouter,
    OpenAi,
    Gemini,
    Anthropic,
    /// Any other OpenAI-compatible endpoint (Ollama, LM Studio, vLLM, ...).
    Custom,
}

impl ProviderKind {
    pub const ALL: [Self; 5] = [Self::OpenRouter, Self::Anthropic, Self::OpenAi, Self::Gemini, Self::Custom];

    /// Lowercase id, as written in the settings file and on the command line.
    pub fn id(self) -> &'static str {
        match self {
            Self::OpenRouter => "openrouter",
            Self::OpenAi => "openai",
            Self::Gemini => "gemini",
            Self::Anthropic => "anthropic",
            Self::Custom => "custom",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::OpenRouter => "OpenRouter",
            Self::OpenAi => "OpenAI",
            Self::Gemini => "Gemini",
            Self::Anthropic => "Anthropic",
            Self::Custom => "Custom (OpenAI-compatible)",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.id().eq_ignore_ascii_case(id))
    }

    /// The fixed endpoint; `None` for a custom one taken from the settings.
    pub fn base_url(self) -> Option<&'static str> {
        match self {
            Self::OpenRouter => Some("https://openrouter.ai/api/v1"),
            Self::OpenAi => Some("https://api.openai.com/v1"),
            Self::Gemini => Some("https://generativelanguage.googleapis.com/v1beta/openai"),
            Self::Anthropic => Some(ANTHROPIC_BASE_URL),
            Self::Custom => None,
        }
    }

    /// The environment variable that conventionally holds this provider's key.
    pub fn key_env(self) -> Option<&'static str> {
        match self {
            Self::OpenRouter => Some("OPENROUTER_API_KEY"),
            Self::OpenAi => Some("OPENAI_API_KEY"),
            Self::Gemini => Some("GEMINI_API_KEY"),
            Self::Anthropic => Some("ANTHROPIC_API_KEY"),
            Self::Custom => None,
        }
    }

    /// Whether requests need a key (local endpoints usually don't).
    pub fn needs_key(self) -> bool {
        self != Self::Custom
    }

    /// The model to preselect when switching to this provider.
    pub fn suggested_model(self) -> Option<&'static str> {
        match self {
            Self::OpenRouter => Some(crate::DEFAULT_MODEL),
            Self::Anthropic => Some("claude-sonnet-5-5"),
            Self::OpenAi | Self::Gemini | Self::Custom => None,
        }
    }
}

/// A client for the configured provider and model.
pub fn connect(settings: &Settings) -> Result<Arc<dyn Provider>> {
    let endpoint = settings.endpoint();
    anyhow::ensure!(!endpoint.is_empty(), "no endpoint configured; run `muninn model`");
    anyhow::ensure!(!settings.model.is_empty(), "no model configured; run `muninn model`");
    let key = match settings.api_key() {
        Some(key) => key,
        None if !settings.provider.needs_key() => String::new(),
        None => anyhow::bail!("no API key for {}; run `muninn model`", settings.provider.name()),
    };
    Ok(match settings.provider {
        ProviderKind::Anthropic => Arc::new(Anthropic::new(endpoint, &key, &settings.model)),
        _ => Arc::new(OpenAiCompatible::new(endpoint, &key, &settings.model)),
    })
}

/// The models the configured provider offers (the model field is ignored).
pub async fn list_models(settings: &Settings) -> Result<Vec<String>> {
    let endpoint = settings.endpoint();
    let key = settings.api_key().unwrap_or_default();
    let models = match settings.provider {
        ProviderKind::Anthropic => Anthropic::new(endpoint, &key, "").models().await,
        _ => OpenAiCompatible::new(endpoint, &key, "").models().await,
    };
    models.with_context(|| format!("cannot list {} models", settings.provider.name()))
}
