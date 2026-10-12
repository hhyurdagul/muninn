//! The terminal app: onboarding, then a conversation in the vault.

mod chat;
mod setup;

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use muninn_core::{Conversation, Decision, Paths, Proposal, Settings, TurnResult, Update};
use muninn_tui::ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use muninn_tui::Terminal;
use tokio::sync::{mpsc, oneshot};

use chat::Chat;
use setup::Setup;

pub enum Start {
    /// Chat, or onboarding when Muninn is not set up yet.
    Default,
    Onboarding,
}

/// Results of background work, delivered to the UI loop.
pub(crate) enum AppEvent {
    Update(Update),
    Review(Proposal, oneshot::Sender<Decision>),
    TurnDone(Box<Conversation>, Result<TurnResult, String>),
    ModelChecked(Result<String, String>),
}

pub(crate) type Tx = mpsc::UnboundedSender<AppEvent>;

enum Screen {
    Setup(Box<Setup>),
    Chat(Box<Chat>),
}

pub async fn run(start: Start) -> Result<()> {
    let paths = Paths::from_env();
    let settings = Settings::load(&paths)?;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut screen = if matches!(start, Start::Onboarding) || !settings.is_complete() {
        Screen::Setup(Box::new(Setup::new(paths.clone(), settings)))
    } else {
        Screen::Chat(Box::new(Chat::new(&paths, &settings, false, tx.clone())?))
    };

    let mut terminal = Terminal::enter()?;
    let mut keys = muninn_tui::spawn_event_reader();
    let mut tick = tokio::time::interval(Duration::from_millis(120));
    loop {
        terminal.inner.draw(|frame| match &mut screen {
            Screen::Setup(setup) => setup.draw(frame),
            Screen::Chat(chat) => chat.draw(frame),
        })?;
        tokio::select! {
            Some(event) = keys.recv() => {
                if let Event::Key(key) = &event {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                }
                match &mut screen {
                    Screen::Setup(setup) => match setup.handle(event, &tx) {
                        setup::Action::None => {}
                        setup::Action::Quit => break,
                        setup::Action::Done(settings) => {
                            screen = Screen::Chat(Box::new(Chat::new(&paths, &settings, true, tx.clone())?));
                        }
                    },
                    Screen::Chat(chat) => {
                        if chat.handle(event) {
                            break;
                        }
                    }
                }
            }
            Some(event) = rx.recv() => match &mut screen {
                Screen::Setup(setup) => setup.on_event(event),
                Screen::Chat(chat) => {
                    if chat.on_event(event) {
                        break;
                    }
                }
            },
            _ = tick.tick() => {}
        }
    }
    Ok(())
}

pub(crate) fn is_ctrl(key: &KeyEvent, c: char) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char(c)
}

/// `~/...` form of a path for display.
pub(crate) fn tilde(path: &Path) -> String {
    match dirs::home_dir().and_then(|home| path.strip_prefix(home).ok().map(Path::to_path_buf)) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}
