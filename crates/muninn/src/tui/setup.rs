//! The plain setup steps of onboarding: vault path, model, agent
//! integration. The rest of onboarding is a conversation (see `chat`).

use std::path::PathBuf;

use muninn_core::{Paths, Settings};
use muninn_tui::ratatui::crossterm::event::{Event, KeyCode};
use muninn_tui::ratatui::layout::{Constraint, Layout};
use muninn_tui::ratatui::style::{Color, Modifier, Style};
use muninn_tui::ratatui::text::{Line, Span};
use muninn_tui::ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use muninn_tui::ratatui::Frame;
use muninn_tui::{centered, Input, Picker};

use super::model::{ModelChoice, Outcome};
use super::{is_ctrl, tilde, AppEvent, Tx};
use crate::integrations::{self, Integration, Status};

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
    model: ModelChoice,
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
        Self {
            paths,
            model: ModelChoice::new(settings.clone()),
            settings,
            step: Step::Vault,
            vault: Input::with_text(vault),
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
                    Step::Model => self.model.handle_paste(&text),
                    Step::Agents => {}
                }
                return Action::None;
            }
            _ => return Action::None,
        };
        if is_ctrl(&key, 'c') {
            return Action::Quit;
        }
        match self.step {
            Step::Vault => match key.code {
                KeyCode::Enter => self.submit_vault(),
                _ => {
                    self.vault.handle_key(key);
                }
            },
            Step::Model => {
                let outcome = self.model.handle_key(key, tx);
                self.model_outcome(outcome);
            }
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
        if self.step == Step::Model {
            let outcome = self.model.on_event(event);
            self.model_outcome(outcome);
        }
    }

    fn model_outcome(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Pending => {}
            Outcome::Cancel => self.go(Step::Vault),
            Outcome::Done(settings) => {
                self.settings = settings;
                match self.settings.save(&self.paths) {
                    Ok(()) => self.go(Step::Agents),
                    Err(e) => self.message = Some((format!("Could not save settings: {e:#}"), true)),
                }
            }
        }
    }

    fn go(&mut self, step: Step) {
        if step == Step::Model {
            self.model = ModelChoice::new(self.settings.clone());
        }
        if step == Step::Agents {
            self.agents = integrations::detect();
            let items = self
                .agents
                .iter()
                .map(|i| match &i.status {
                    Status::Missing => i.agent.name().to_string(),
                    Status::Current => format!("{}  (already set up)", i.agent.name()),
                    Status::Stale(old) => format!("{}  (set up with another muninn: {old})", i.agent.name()),
                })
                .collect();
            let checks = self.agents.iter().map(|i| i.status != Status::Current).collect();
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
        let area = centered(frame.area(), 84, 26);
        frame.render_widget(Clear, area);
        let (number, title) = match self.step {
            Step::Vault => (1, "Where is your vault?"),
            Step::Model => (2, "Connect a model"),
            Step::Agents => (3, "Reach the vault from your coding agents"),
        };
        let accent = Style::default().fg(Color::Magenta);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(accent)
            .padding(muninn_tui::ratatui::widgets::Padding::horizontal(1))
            .title(Line::from(vec![
                Span::styled(
                    " Muninn setup ",
                    Style::default().fg(Color::Black).bg(Color::Magenta).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" {number}/3 · {title} "), Style::default().add_modifier(Modifier::BOLD)),
            ]))
            .title_bottom(Span::styled(
                match self.step {
                    Step::Vault => " Enter continue · Ctrl+C quit ",
                    Step::Model => self.model.hints(),
                    Step::Agents => " Space toggle · Enter continue · Esc back ",
                },
                Style::default().fg(Color::DarkGray),
            ));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let [intro, body, message] =
            Layout::vertical([Constraint::Length(4), Constraint::Min(4), Constraint::Length(5)]).areas(inner);

        let intro_text = match self.step {
            Step::Vault => {
                "Pick the folder that holds your notes. Muninn only reads and writes inside it. \
An empty or new folder starts a vault from zero."
            }
            Step::Model => {
                "Pick a provider and a model. Keys are stored in Muninn's own settings, never in \
the vault; the provider's usual environment variable works too."
            }
            Step::Agents => {
                "Muninn can register `muninn mcp` with the agents installed here, so they can \
save to and ask your vault from any project."
            }
        };
        frame.render_widget(Paragraph::new(intro_text).wrap(Wrap { trim: true }), intro);

        match self.step {
            Step::Vault => self.vault.render(frame, field_area(body, 0), field_block("Vault folder", true)),
            Step::Model => {
                let area = muninn_tui::ratatui::layout::Rect { height: body.height + message.height, ..body };
                return self.model.draw(frame, area);
            }
            Step::Agents if self.agents.is_empty() => {
                frame.render_widget(
                    Paragraph::new("No supported agents (Claude Code, Codex, omp) found on PATH."),
                    body,
                );
            }
            Step::Agents => self.picker.render(frame, body, Block::default()),
        }

        if let Some((text, is_error)) = &self.message {
            let style = if *is_error { Style::default().fg(Color::Red) } else { Style::default().fg(Color::Green) };
            frame.render_widget(Paragraph::new(text.clone()).style(style).wrap(Wrap { trim: false }), message);
        }
    }
}

fn field_area(body: muninn_tui::ratatui::layout::Rect, index: usize) -> muninn_tui::ratatui::layout::Rect {
    let y = body.y + 3 * index as u16;
    muninn_tui::ratatui::layout::Rect { y, height: 3.min(body.bottom().saturating_sub(y)), ..body }
}

fn field_block(label: &str, focused: bool) -> Block<'static> {
    let style = if focused { Style::default().fg(Color::Magenta) } else { Style::default().fg(Color::DarkGray) };
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(style)
        .title(Span::styled(format!(" {label} "), style.add_modifier(Modifier::BOLD)))
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
