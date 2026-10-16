//! Choosing a provider and model: provider, then endpoint (custom only) and
//! key when needed, then a model from the provider's list. Nothing is sent to
//! the model until the first message. Used by onboarding and by `/model`.

use muninn_core::{list_models, ProviderKind, Settings};
use muninn_tui::ratatui::crossterm::event::{KeyCode, KeyEvent};
use muninn_tui::ratatui::layout::{Constraint, Layout, Rect};
use muninn_tui::ratatui::style::{Color, Modifier, Style};
use muninn_tui::ratatui::text::{Line, Span};
use muninn_tui::ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use muninn_tui::ratatui::Frame;
use muninn_tui::{Input, Picker};

use super::{AppEvent, Tx};

pub enum Outcome {
    Pending,
    Cancel,
    /// The chosen provider and model; the caller saves the settings.
    Done(Settings),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Provider,
    Endpoint,
    Key,
    Model,
}

pub struct ModelChoice {
    settings: Settings,
    stage: Stage,
    providers: Picker,
    endpoint: Input,
    key: Input,
    filter: Input,
    models: Vec<String>,
    /// The models matching the filter.
    list: Picker,
    busy: bool,
    message: Option<(String, bool)>,
}

impl ModelChoice {
    pub fn new(settings: Settings) -> Self {
        let items = ProviderKind::ALL.iter().map(|&kind| provider_label(&settings, kind)).collect();
        let mut providers = Picker::new(items);
        providers.select(ProviderKind::ALL.iter().position(|&k| k == settings.provider).unwrap_or(0));
        Self {
            endpoint: Input::with_text(settings.base_url.clone()),
            settings,
            stage: Stage::Provider,
            providers,
            key: Input::default().masked(),
            filter: Input::default(),
            models: Vec::new(),
            list: Picker::new(Vec::new()),
            busy: false,
            message: None,
        }
    }

    pub fn handle_paste(&mut self, text: &str) {
        match self.stage {
            Stage::Endpoint => self.endpoint.insert_str(text.trim()),
            Stage::Key => self.key.insert_str(text.trim()),
            Stage::Model => {
                self.filter.insert_str(text.trim());
                self.refilter();
            }
            Stage::Provider => {}
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, tx: &Tx) -> Outcome {
        if self.busy {
            return Outcome::Pending;
        }
        match (self.stage, key.code) {
            (Stage::Provider, KeyCode::Esc) => return Outcome::Cancel,
            (Stage::Provider, KeyCode::Enter) => {
                let kind = ProviderKind::ALL[self.providers.selected().unwrap_or(0)];
                if kind != self.settings.provider {
                    self.settings.provider = kind;
                    self.settings.model = kind.suggested_model().unwrap_or_default().to_string();
                }
                if kind == ProviderKind::Custom {
                    self.go(Stage::Endpoint);
                } else if self.settings.api_key().is_none() {
                    self.go(Stage::Key);
                } else {
                    self.fetch(tx);
                }
            }
            (Stage::Provider, _) => {
                self.providers.handle_key(key);
            }
            (Stage::Endpoint, KeyCode::Esc) => self.go(Stage::Provider),
            (Stage::Endpoint, KeyCode::Enter) => {
                let url = self.endpoint.text().trim().trim_end_matches('/').to_string();
                if url.is_empty() {
                    self.message = Some(("Enter the endpoint's base URL, ending in /v1.".into(), true));
                } else {
                    self.settings.base_url = url;
                    self.go(Stage::Key);
                }
            }
            (Stage::Endpoint, _) => {
                self.endpoint.handle_key(key);
            }
            (Stage::Key, KeyCode::Esc) => {
                self.go(if self.settings.provider == ProviderKind::Custom { Stage::Endpoint } else { Stage::Provider })
            }
            (Stage::Key, KeyCode::Enter) => {
                let provider = self.settings.provider;
                self.settings.set_key(provider, self.key.text());
                if self.settings.api_key().is_none() && provider.needs_key() {
                    self.message = Some((format!("{} needs an API key.", provider.name()), true));
                } else {
                    self.fetch(tx);
                }
            }
            (Stage::Key, _) => {
                self.key.handle_key(key);
            }
            (Stage::Model, KeyCode::Esc) => self.go(Stage::Key),
            (Stage::Model, KeyCode::Enter) => {
                let chosen = match self.list.selected().and_then(|i| self.list.items.get(i)) {
                    Some(model) => model.clone(),
                    None => self.filter.text().trim().to_string(),
                };
                if chosen.is_empty() {
                    self.message = Some(("Type a model id.".into(), true));
                } else {
                    self.settings.model = chosen;
                    return Outcome::Done(self.settings.clone());
                }
            }
            (Stage::Model, KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown) => {
                let steps = if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) { 10 } else { 1 };
                let code = if matches!(key.code, KeyCode::Up | KeyCode::PageUp) { KeyCode::Up } else { KeyCode::Down };
                for _ in 0..steps {
                    self.list.handle_key(KeyEvent::from(code));
                }
            }
            (Stage::Model, _) => {
                if self.filter.handle_key(key) {
                    self.refilter();
                }
            }
        }
        Outcome::Pending
    }

    /// The model list, fetched in the background.
    pub fn on_event(&mut self, event: AppEvent) -> Outcome {
        match event {
            AppEvent::Models(result) => {
                self.busy = false;
                self.go(Stage::Model);
                match result {
                    Ok(models) => self.models = models,
                    Err(e) => {
                        self.models.clear();
                        self.message = Some((format!("{e}\nType a model id, or Esc to change the key."), true));
                    }
                }
                self.filter.set_text("");
                self.refilter();
                let current = self.models.iter().position(|m| *m == self.settings.model);
                match current {
                    Some(index) => self.list.select(index),
                    None => self.filter.set_text(self.settings.model.clone()),
                }
                if current.is_none() {
                    self.refilter();
                }
                Outcome::Pending
            }
            _ => Outcome::Pending,
        }
    }

    fn go(&mut self, stage: Stage) {
        self.stage = stage;
        self.message = None;
        if stage == Stage::Key {
            let provider = self.settings.provider;
            self.key.set_text(self.settings.api_keys.get(provider.id()).cloned().unwrap_or_default());
            if let Some(url) = provider.key_url() {
                self.message = Some((format!("Get a key at {url}"), false));
            }
        }
    }

    fn fetch(&mut self, tx: &Tx) {
        self.busy = true;
        self.message = Some((format!("Fetching {} models…", self.settings.provider.name()), false));
        let (settings, tx) = (self.settings.clone(), tx.clone());
        tokio::spawn(async move {
            let result = list_models(&settings).await.map_err(|e| format!("{e:#}"));
            let _ = tx.send(AppEvent::Models(result));
        });
    }

    fn refilter(&mut self) {
        let needle = self.filter.text().trim().to_lowercase();
        let items = self.models.iter().filter(|m| m.to_lowercase().contains(&needle)).cloned().collect();
        self.list = Picker::new(items);
    }

    pub fn hints(&self) -> &'static str {
        match self.stage {
            Stage::Provider => " ↑↓ select · Enter continue · Esc cancel ",
            Stage::Endpoint | Stage::Key => " Enter continue · Esc back ",
            Stage::Model => " type to filter · ↑↓ select · Enter use · Esc back ",
        }
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let [body, message] = Layout::vertical([Constraint::Min(3), Constraint::Length(3)]).areas(area);
        let provider = self.settings.provider;
        match self.stage {
            Stage::Provider => self.providers.render(frame, body, Block::default()),
            Stage::Endpoint => self.endpoint.render(
                frame,
                field(body, 0),
                field_block("Endpoint (OpenAI-compatible, e.g. http://localhost:11434/v1)"),
            ),
            Stage::Key => {
                let label = match provider.key_env() {
                    Some(var) => format!("{} API key (or set ${var})", provider.name()),
                    None => format!("{} API key (optional)", provider.name()),
                };
                self.key.render(frame, field(body, 0), field_block(&label));
            }
            Stage::Model => {
                let [filter, list] = Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).areas(body);
                let label = format!("{} model · {} of {}", provider.name(), self.list.items.len(), self.models.len());
                self.filter.render(frame, filter, field_block(&label));
                self.list.render(frame, list, Block::default());
            }
        }
        if let Some((text, is_error)) = &self.message {
            let style = if *is_error { Style::default().fg(Color::Red) } else { Style::default().fg(Color::Green) };
            frame.render_widget(Paragraph::new(text.clone()).style(style).wrap(Wrap { trim: false }), message);
        }
    }
}

fn provider_label(settings: &Settings, kind: ProviderKind) -> String {
    let current = if kind == settings.provider { "  ← current" } else { "" };
    let key = match (settings.api_keys.contains_key(kind.id()), kind.key_env()) {
        (true, _) => "  (key saved)".to_string(),
        (false, Some(var)) if std::env::var(var).is_ok_and(|v| !v.is_empty()) => format!("  (key from ${var})"),
        _ => String::new(),
    };
    format!("{:<28}{key}{current}", kind.name())
}

fn field(body: Rect, index: u16) -> Rect {
    let y = body.y + 3 * index;
    Rect { y, height: 3.min(body.bottom().saturating_sub(y)), ..body }
}

fn field_block(label: &str) -> Block<'static> {
    let style = Style::default().fg(Color::Magenta);
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(style)
        .title(Span::styled(format!(" {label} "), style.add_modifier(Modifier::BOLD)))
}

/// Title line for a box that hosts the chooser.
pub fn title(text: &str) -> Line<'static> {
    Line::from(Span::styled(format!(" {text} "), Style::default().add_modifier(Modifier::BOLD)))
}
