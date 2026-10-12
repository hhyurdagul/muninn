//! The plain setup steps of onboarding: vault path, model, agent
//! integration. The rest of onboarding is a conversation (see `chat`).

use std::path::PathBuf;

use muninn_core::{check_model, Paths, Settings};
use muninn_tui::ratatui::crossterm::event::{Event, KeyCode};
use muninn_tui::ratatui::layout::{Constraint, Layout};
use muninn_tui::ratatui::style::{Color, Modifier, Style};
use muninn_tui::ratatui::text::{Line, Span};
use muninn_tui::ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use muninn_tui::ratatui::Frame;
use muninn_tui::{centered, Input, Picker};

use super::{is_ctrl, tilde, AppEvent, Tx};
use crate::integrations::{self, Integration};

pub enum Action {
    None,
    Quit,
    Done(Settings),
}

#[derive(PartialEq, Eq)]
enum Step {
    Vault,
    Model,
    Agents,
}

pub struct Setup {
    paths: Paths,
    settings: Settings,
    step: Step,
    vault: Input,
    /// Base URL, model, API key.
    fields: [Input; 3],
    focus: usize,
    checking: bool,
    message: Option<(String, bool)>,
    agents: Vec<Integration>,
    picker: Picker,
    /// Set once the selected agents were configured.
    installed: bool,
}

impl Setup {
    pub fn new(paths: Paths, settings: Settings) -> Self {
        let vault = settings
            .vault
            .as_deref()
            .map(tilde)
            .unwrap_or_else(|| std::env::current_dir().map(|d| tilde(&d)).unwrap_or_default());
        let fields = [
            Input::with_text(settings.base_url.clone()),
            Input::with_text(settings.model.clone()),
            Input::with_text(settings.api_key.clone().unwrap_or_default()).masked(),
        ];
        Self {
            paths,
            settings,
            step: Step::Vault,
            vault: Input::with_text(vault),
            fields,
            focus: 2,
            checking: false,
            message: None,
            agents: Vec::new(),
            picker: Picker::new(Vec::new()),
            installed: false,
        }
    }

    pub fn handle(&mut self, event: Event, tx: &Tx) -> Action {
        let key = match event {
            Event::Key(key) => key,
            Event::Paste(text) => {
                match self.step {
                    Step::Vault => self.vault.insert_str(text.trim()),
                    Step::Model => self.fields[self.focus].insert_str(text.trim()),
                    Step::Agents => {}
                }
                return Action::None;
            }
            _ => return Action::None,
        };
        if is_ctrl(&key, 'c') {
            return Action::Quit;
        }
        if self.checking {
            return Action::None;
        }
        match self.step {
            Step::Vault => match key.code {
                KeyCode::Enter => self.submit_vault(),
                _ => {
                    self.vault.handle_key(key);
                }
            },
            Step::Model => match key.code {
                KeyCode::Enter => self.submit_model(tx),
                KeyCode::Esc => self.go(Step::Vault),
                KeyCode::Tab | KeyCode::Down => self.focus = (self.focus + 1) % 3,
                KeyCode::BackTab | KeyCode::Up => self.focus = (self.focus + 2) % 3,
                _ => {
                    self.fields[self.focus].handle_key(key);
                }
            },
            Step::Agents => match key.code {
                KeyCode::Enter if self.installed || self.agents.is_empty() => {
                    return Action::Done(self.settings.clone());
                }
                KeyCode::Enter => self.install_agents(),
                KeyCode::Esc => self.go(Step::Model),
                _ => {
                    self.picker.handle_key(key);
                }
            },
        }
        Action::None
    }

    pub fn on_event(&mut self, event: AppEvent) {
        let AppEvent::ModelChecked(result) = event else { return };
        self.checking = false;
        match result {
            Ok(_) => match self.settings.save(&self.paths) {
                Ok(()) => self.go(Step::Agents),
                Err(e) => self.message = Some((format!("Could not save settings: {e:#}"), true)),
            },
            Err(e) => self.message = Some((format!("Test call failed: {e}"), true)),
        }
    }

    fn go(&mut self, step: Step) {
        if step == Step::Agents {
            self.agents = integrations::detect();
            let items = self
                .agents
                .iter()
                .map(|i| format!("{}{}", i.agent.name(), if i.installed { "  (already set up)" } else { "" }))
                .collect();
            let checks = self.agents.iter().map(|i| !i.installed).collect();
            self.picker = Picker::new(items).with_checks(checks);
            self.installed = false;
        }
        self.step = step;
        self.message = None;
    }

    fn submit_vault(&mut self) {
        let raw = self.vault.text().trim();
        if raw.is_empty() {
            self.message = Some(("Enter the folder of your vault.".into(), true));
            return;
        }
        let path = expand(raw);
        if path.exists() && !path.is_dir() {
            self.message = Some((format!("{} is a file, not a folder.", path.display()), true));
            return;
        }
        if let Err(e) = std::fs::create_dir_all(&path) {
            self.message = Some((format!("Cannot create {}: {e}", path.display()), true));
            return;
        }
        self.settings.vault = Some(path.canonicalize().unwrap_or(path));
        self.go(Step::Model);
    }

    fn submit_model(&mut self, tx: &Tx) {
        let [base_url, model, key] = &self.fields;
        if model.text().trim().is_empty() || base_url.text().trim().is_empty() {
            self.message = Some(("Endpoint and model are required.".into(), true));
            return;
        }
        self.settings.base_url = base_url.text().trim().to_string();
        self.settings.model = model.text().trim().to_string();
        self.settings.api_key = Some(key.text().trim().to_string()).filter(|k| !k.is_empty());
        if self.settings.api_key().is_none() {
            self.message = Some(("An API key is required.".into(), true));
            return;
        }
        self.checking = true;
        self.message = Some(("Making a test call…".into(), false));
        let (settings, tx) = (self.settings.clone(), tx.clone());
        tokio::spawn(async move {
            let result = check_model(&settings).await.map_err(|e| format!("{e:#}"));
            let _ = tx.send(AppEvent::ModelChecked(result));
        });
    }

    fn install_agents(&mut self) {
        let checked = self.picker.checked.clone().unwrap_or_default();
        let mut report = Vec::new();
        for (integration, _) in self.agents.iter().zip(checked).filter(|(_, on)| *on) {
            let name = integration.agent.name();
            match integrations::install(integration.agent) {
                Ok(()) => report.push(format!("{name}: added `muninn mcp`")),
                Err(e) => report.push(format!("{name}: failed ({e:#})")),
            }
        }
        if report.is_empty() {
            report.push("Skipped.".into());
        }
        self.installed = true;
        self.message = Some((format!("{}\nPress Enter to continue.", report.join("\n")), false));
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        let area = centered(frame.area(), 84, 22);
        frame.render_widget(Clear, area);
        let (number, title) = match self.step {
            Step::Vault => (1, "Where is your vault?"),
            Step::Model => (2, "Connect a model"),
            Step::Agents => (3, "Reach the vault from your coding agents"),
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" Muninn setup · {number}/3 · {title} "))
            .title_bottom(match self.step {
                Step::Vault => " Enter continue · Ctrl+C quit ",
                Step::Model => " Tab next field · Enter test & continue · Esc back ",
                Step::Agents => " Space toggle · Enter continue · Esc back ",
            });
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let [intro, body, message] =
            Layout::vertical([Constraint::Length(4), Constraint::Min(4), Constraint::Length(5)]).areas(inner);

        let intro_text = match self.step {
            Step::Vault => "Pick the folder that holds your notes. Muninn only reads and writes inside it. \
An empty or new folder starts a vault from zero.",
            Step::Model => "Muninn talks to any OpenAI-compatible endpoint. The key is stored in Muninn's \
own settings, never in the vault.",
            Step::Agents => "Muninn can register `muninn mcp` with the agents installed here, so they can \
save to and ask your vault from any project.",
        };
        frame.render_widget(Paragraph::new(intro_text).wrap(Wrap { trim: true }), intro);

        match self.step {
            Step::Vault => self.vault.render(frame, field_area(body, 0), field_block("Vault folder", true)),
            Step::Model => {
                for (i, label) in ["Endpoint", "Model", "API key"].into_iter().enumerate() {
                    let block = field_block(label, i == self.focus);
                    let area = field_area(body, i);
                    if i == self.focus {
                        self.fields[i].render(frame, area, block);
                    } else {
                        let text = if self.fields[i].masked { "•".repeat(self.fields[i].text().chars().count()) } else { self.fields[i].text().to_string() };
                        frame.render_widget(Paragraph::new(text).block(block), area);
                    }
                }
            }
            Step::Agents if self.agents.is_empty() => {
                frame.render_widget(Paragraph::new("No supported agents (Claude Code, Codex, omp) found on PATH."), body);
            }
            Step::Agents => self.picker.render(frame, body, Block::default()),
        }

        if let Some((text, is_error)) = &self.message {
            let style = if *is_error { Style::default().fg(Color::Red) } else { Style::default().fg(Color::Green) };
            frame.render_widget(Paragraph::new(Span::styled(text.clone(), style)).wrap(Wrap { trim: false }), message);
        }
    }
}

fn field_area(body: muninn_tui::ratatui::layout::Rect, index: usize) -> muninn_tui::ratatui::layout::Rect {
    let y = body.y + 3 * index as u16;
    muninn_tui::ratatui::layout::Rect { y, height: 3.min(body.bottom().saturating_sub(y)), ..body }
}

fn field_block(label: &str, focused: bool) -> Block<'static> {
    let style = if focused { Style::default().add_modifier(Modifier::BOLD) } else { Style::default().fg(Color::DarkGray) };
    Block::default().borders(Borders::ALL).border_style(style).title(Line::from(format!(" {label} ")))
}

/// Expands `~` and makes the path absolute.
fn expand(raw: &str) -> PathBuf {
    let path = match raw.strip_prefix("~") {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest.trim_start_matches('/')),
        None => PathBuf::from(raw),
    };
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    }
}
